//! `/catalog` — a fullscreen popup for browsing, installing, and removing the
//! optional tools and commands hosted in the catalog. Tools are unchecked by
//! default; applying only acts on items the user explicitly toggled, so
//! already-installed items are preserved unless the user unchecks them.
//!
//! The list/detail rendering is shared with the onboarding wizard via
//! [`crate::ui::picker`]; the onboarding "catalog" step reuses
//! [`build_items`] and [`apply`] so both paths behave identically.

use std::io;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::ext::catalog::{self, CatalogEntry};
use crate::ui::fullscreen::{self, FullscreenTerminal};
use crate::ui::picker::{self, ACCENT, BG, BORDER, DIM, Item, MUTED, TEXT};

/// Result of running the popup.
pub struct Outcome {
    /// True if any item was installed or removed.
    pub changed: bool,
    /// A one-line summary suitable for the chat transcript.
    pub message: String,
}

/// Build picker rows for the given catalog entries. Tools are unchecked by
/// default; already-installed items are preserved on apply unless the user
/// explicitly unchecks them.
///
/// Items whose on-disk content differs from the catalog are tagged "update" and
/// pre-checked (so a plain Enter pulls every pending update at once).
pub fn build_items(entries: &[CatalogEntry]) -> Vec<Item> {
    entries
        .iter()
        .map(|e| {
            let update = catalog::is_installed(e) && catalog::needs_update(e);
            let mut item = Item::new(e.name.clone(), e.description.clone(), false);
            item.category = if e.kind == "command" {
                "command"
            } else {
                "tool"
            };
            if update {
                item.tag = Some("update".to_string());
                item.checked = true;
                item.user_touched = true;
            }
            item
        })
        .collect()
}

/// Apply a checklist against the catalog.
///
/// When `touched_only` is true (catalog UI), only items the user explicitly
/// toggled are acted on — untouched items keep their current install state.
/// When false (onboarding), all items are applied as-is.
///
/// Returns `(installed, removed, errors)` counts.
pub fn apply(
    entries: &[CatalogEntry],
    items: &[Item],
    touched_only: bool,
) -> (usize, usize, Vec<String>) {
    let mut installed = 0;
    let mut removed = 0;
    let mut errors = Vec::new();
    for (entry, item) in entries.iter().zip(items.iter()) {
        let on_disk = catalog::is_installed(entry);
        let apply = if touched_only {
            item.user_touched
        } else {
            true
        };
        if !apply {
            continue;
        }
        if item.checked {
            if !on_disk || catalog::needs_update(entry) {
                match catalog::install(entry) {
                    Ok(()) => installed += 1,
                    Err(e) => errors.push(e),
                }
            }
        } else if on_disk {
            match catalog::remove(entry) {
                Ok(()) => removed += 1,
                Err(e) => errors.push(e),
            }
        }
    }
    (installed, removed, errors)
}

struct State {
    entries: Vec<CatalogEntry>,
    items: Vec<Item>,
    cursor: usize,
    outcome: Outcome,
}

impl State {
    fn new() -> Self {
        let entries = catalog::sync_quiet();
        let items = build_items(&entries);
        Self {
            entries,
            items,
            cursor: 0,
            outcome: Outcome {
                changed: false,
                message: "Catalog closed.".to_string(),
            },
        }
    }
}

/// Run the catalog popup fullscreen. Returns the outcome (whether anything
/// changed + a summary message).
pub fn run() -> io::Result<Outcome> {
    fullscreen::run(run_loop)
}

fn run_loop(term: &mut FullscreenTerminal) -> io::Result<Outcome> {
    let mut state = State::new();
    term.draw(|frame| draw(frame, &state))?;

    loop {
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Esc => return Ok(state.outcome),
                KeyCode::Up | KeyCode::Char('k') => move_cursor(&mut state, -1),
                KeyCode::Down | KeyCode::Char('j') => move_cursor(&mut state, 1),
                KeyCode::Char(' ') => toggle(&mut state),
                KeyCode::Char('a') => set_all(&mut state, true),
                KeyCode::Char('n') => set_all(&mut state, false),
                KeyCode::Enter => {
                    apply_state(&mut state);
                    return Ok(state.outcome);
                }
                _ => {}
            },
            Event::Resize(_, _) => {}
            _ => {}
        }
        term.draw(|frame| draw(frame, &state))?;
    }
}

fn move_cursor(state: &mut State, delta: i32) {
    let len = state.items.len() as i32;
    if len == 0 {
        return;
    }
    state.cursor = ((state.cursor as i32 + delta).rem_euclid(len)) as usize;
}

fn toggle(state: &mut State) {
    if let Some(item) = state.items.get_mut(state.cursor) {
        item.checked = !item.checked;
        item.user_touched = true;
    }
}

fn set_all(state: &mut State, checked: bool) {
    for item in state.items.iter_mut() {
        item.checked = checked;
        item.user_touched = true;
    }
}

fn apply_state(state: &mut State) {
    let (installed, removed, errors) = apply(&state.entries, &state.items, true);
    state.outcome.changed = installed > 0 || removed > 0;
    let mut parts = Vec::new();
    if installed > 0 {
        parts.push(format!("installed {installed}"));
    }
    if removed > 0 {
        parts.push(format!("removed {removed}"));
    }
    let mut msg = if parts.is_empty() {
        "Catalog: no changes.".to_string()
    } else {
        format!("Catalog: {}.", parts.join(", "))
    };
    if !errors.is_empty() {
        msg.push_str(&format!(" {} failed.", errors.len()));
    }
    state.outcome.message = msg;
}

// ---- rendering ----------------------------------------------------------

fn draw(frame: &mut ratatui::Frame, state: &State) {
    let screen = frame.area();
    frame.render_widget(Block::default().style(Style::default().bg(BG)), screen);

    let area = screen;

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header
            Constraint::Min(1),    // body
            Constraint::Length(1), // footer
        ])
        .split(area);

    draw_header(frame, chunks[0]);
    draw_body(frame, chunks[1], state);
    draw_footer(frame, chunks[2]);
}

fn draw_header(frame: &mut ratatui::Frame, area: Rect) {
    let lines = vec![
        Line::from(vec![
            Span::styled(
                "bone ",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::styled("catalog", Style::default().fg(MUTED)),
        ]),
        Line::from(Span::styled(
            "Optional tools & commands — download on demand",
            Style::default().fg(DIM),
        )),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(BORDER))
                .padding(ratatui::widgets::Padding::new(2, 0, 1, 0)),
        ),
        area,
    );
}

fn draw_body(frame: &mut ratatui::Frame, area: Rect, state: &State) {
    if state.items.is_empty() {
        let lines = vec![
            Line::from(Span::styled(
                "No catalog items available.",
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "bone couldn't reach the catalog (you may be offline). Anything \
                 already installed still works; try again later.",
                Style::default().fg(MUTED),
            )),
        ];
        frame.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: false }),
            picker::pad(area),
        );
        return;
    }
    picker::draw_list(
        frame,
        area,
        "Tools & commands",
        "Check to install, uncheck to remove. Toggle with Space; Enter applies.",
        &state.items,
        state.cursor,
    );
}

fn draw_footer(frame: &mut ratatui::Frame, area: Rect) {
    let mut keys: Vec<Span> = Vec::new();
    let mut push = |k: &str, label: &str| {
        keys.push(Span::styled(
            format!(" {k} "),
            Style::default()
                .fg(BG)
                .bg(MUTED)
                .add_modifier(Modifier::BOLD),
        ));
        keys.push(Span::styled(
            format!(" {label}   "),
            Style::default().fg(DIM),
        ));
    };
    push("↑↓", "move");
    push("space", "toggle");
    push("a/n", "all/none");
    push("enter", "apply");
    push("esc", "close");

    frame.render_widget(
        Paragraph::new(Line::from(keys))
            .alignment(Alignment::Left)
            .block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_style(Style::default().fg(BORDER)),
            ),
        area,
    );
}

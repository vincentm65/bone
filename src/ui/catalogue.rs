//! `/catalogue` — a fullscreen popup for browsing, installing, and removing the
//! optional tools and commands hosted in the catalog. A checked box means the
//! item is installed (present in `~/.bone-rust/lua/{tools,commands}/`); applying
//! downloads newly-checked items and deletes newly-unchecked ones.
//!
//! The list/detail rendering is shared with the onboarding wizard via
//! [`crate::ui::picker`]; the onboarding "catalogue" step reuses
//! [`build_items`] and [`apply`] so both paths behave identically.

use std::io;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::ext::catalog::{self, CatalogEntry};
use crate::ui::fullscreen::{self, FullscreenTerminal};
use crate::ui::picker::{self, Item, ACCENT, BG, BORDER, DIM, MUTED, TEXT};

/// Result of running the popup.
pub struct Outcome {
    /// True if any item was installed or removed.
    pub changed: bool,
    /// A one-line summary suitable for the chat transcript.
    pub message: String,
}

/// Build picker rows for the given catalog entries, marking installed items
/// checked and flagging those with a newer available version.
pub fn build_items(entries: &[CatalogEntry]) -> Vec<Item> {
    entries
        .iter()
        .map(|e| {
            let installed = catalog::is_installed(e);
            let update = installed
                && catalog::installed_version(&e.name).is_some_and(|v| e.version > v);
            let mut item = Item::new(e.name.clone(), e.description.clone(), installed);
            item.category = if e.kind == "command" { "command" } else { "tool" };
            if update {
                item.tag = Some("update".to_string());
            }
            item
        })
        .collect()
}

/// Apply a checklist against the catalog: install newly-checked / updated items,
/// remove newly-unchecked ones. Returns `(installed, removed, errors)` counts.
pub fn apply(entries: &[CatalogEntry], items: &[Item]) -> (usize, usize, Vec<String>) {
    let mut installed = 0;
    let mut removed = 0;
    let mut errors = Vec::new();
    for (entry, item) in entries.iter().zip(items.iter()) {
        let on_disk = catalog::is_installed(entry);
        if item.checked {
            let outdated = catalog::installed_version(&entry.name).is_some_and(|v| entry.version > v);
            if !on_disk || outdated {
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
                message: "Catalogue closed.".to_string(),
            },
        }
    }
}

/// Run the catalogue popup fullscreen. Returns the outcome (whether anything
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
    }
}

fn set_all(state: &mut State, checked: bool) {
    for item in state.items.iter_mut() {
        item.checked = checked;
    }
}

fn apply_state(state: &mut State) {
    let (installed, removed, errors) = apply(&state.entries, &state.items);
    state.outcome.changed = installed > 0 || removed > 0;
    let mut parts = Vec::new();
    if installed > 0 {
        parts.push(format!("installed {installed}"));
    }
    if removed > 0 {
        parts.push(format!("removed {removed}"));
    }
    let mut msg = if parts.is_empty() {
        "Catalogue: no changes.".to_string()
    } else {
        format!("Catalogue: {}.", parts.join(", "))
    };
    if !errors.is_empty() {
        msg.push_str(&format!(" {} failed.", errors.len()));
    }
    state.outcome.message = msg;
}

// ---- rendering ----------------------------------------------------------

fn draw(frame: &mut ratatui::Frame, state: &State) {
    let screen = frame.area();
    frame.render_widget(
        Block::default().style(Style::default().bg(BG)),
        screen,
    );

    let width = screen.width.min(76);
    let area = Rect {
        x: screen.x + (screen.width.saturating_sub(width)) / 2,
        y: screen.y,
        width,
        height: screen.height,
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4), // header
            Constraint::Min(6),    // body
            Constraint::Length(2), // footer
        ])
        .split(area);

    draw_header(frame, chunks[0]);
    draw_body(frame, chunks[1], state);
    draw_footer(frame, chunks[2]);
}

fn draw_header(frame: &mut ratatui::Frame, area: Rect) {
    let lines = vec![
        Line::from(vec![
            Span::styled("bone ", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
            Span::styled("catalogue", Style::default().fg(MUTED)),
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
                "No catalogue items available.",
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "bone couldn't reach the catalogue (you may be offline). Anything \
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
            Style::default().fg(BG).bg(MUTED).add_modifier(Modifier::BOLD),
        ));
        keys.push(Span::styled(format!(" {label}   "), Style::default().fg(DIM)));
    };
    push("↑↓", "move");
    push("space", "toggle");
    push("a/n", "all/none");
    push("enter", "apply");
    push("esc", "close");

    frame.render_widget(
        Paragraph::new(Line::from(keys)).alignment(Alignment::Left).block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(BORDER)),
        ),
        area,
    );
}

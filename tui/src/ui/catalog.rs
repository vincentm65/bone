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
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::ext::catalog::{self, CatalogEntry};
use crate::ui::fullscreen::{self, FullscreenTerminal};
use crate::ui::picker::{self, ACCENT, BAD, BG, BORDER, DIM, GOOD, Item, MUTED, TEXT};

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

/// Per-row outcome of an apply pass, aligned 1:1 with the items slice.
#[derive(Clone)]
pub enum ItemResult {
    /// No action taken (untouched, or already in the desired state).
    Unchanged,
    Installed,
    Removed,
    Failed(String),
}

/// Apply a checklist against the catalog, returning a per-row [`ItemResult`]
/// aligned 1:1 with `items`.
///
/// When `touched_only` is true (catalog UI), only items the user explicitly
/// toggled are acted on — untouched items keep their current install state.
/// When false (onboarding), all items are applied as-is.
pub fn apply_results(
    entries: &[CatalogEntry],
    items: &[Item],
    touched_only: bool,
) -> Vec<ItemResult> {
    entries
        .iter()
        .zip(items.iter())
        .map(|(entry, item)| {
            let on_disk = catalog::is_installed(entry);
            let act = if touched_only {
                item.user_touched
            } else {
                true
            };
            if !act {
                return ItemResult::Unchanged;
            }
            if item.checked {
                if !on_disk || catalog::needs_update(entry) {
                    match catalog::install(entry) {
                        Ok(()) => ItemResult::Installed,
                        Err(e) => ItemResult::Failed(e),
                    }
                } else {
                    ItemResult::Unchanged
                }
            } else if on_disk {
                match catalog::remove(entry) {
                    Ok(()) => ItemResult::Removed,
                    Err(e) => ItemResult::Failed(e),
                }
            } else {
                ItemResult::Unchanged
            }
        })
        .collect()
}

/// Apply a checklist against the catalog, returning `(installed, removed,
/// errors)` counts. Thin wrapper over [`apply_results`] for callers (onboarding)
/// that only need totals.
pub fn apply(
    entries: &[CatalogEntry],
    items: &[Item],
    touched_only: bool,
) -> (usize, usize, Vec<String>) {
    let mut installed = 0;
    let mut removed = 0;
    let mut errors = Vec::new();
    for r in apply_results(entries, items, touched_only) {
        match r {
            ItemResult::Installed => installed += 1,
            ItemResult::Removed => removed += 1,
            ItemResult::Failed(e) => errors.push(e),
            ItemResult::Unchanged => {}
        }
    }
    (installed, removed, errors)
}

struct State {
    entries: Vec<CatalogEntry>,
    items: Vec<Item>,
    cursor: usize,
    outcome: Outcome,
    /// Set once the user has applied changes: a one-line banner summarizing what
    /// happened. `Some` switches the screen into its read-only "result" phase.
    result: Option<String>,
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
            result: None,
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
            // Result phase: any of esc/enter closes; cursor still moves so the
            // user can scroll the list and read per-item status / errors.
            Event::Key(key) if key.kind == KeyEventKind::Press && state.result.is_some() => {
                match key.code {
                    KeyCode::Esc | KeyCode::Enter => return Ok(state.outcome),
                    KeyCode::Up | KeyCode::Char('k') => move_cursor(&mut state, -1),
                    KeyCode::Down | KeyCode::Char('j') => move_cursor(&mut state, 1),
                    _ => {}
                }
            }
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Esc => return Ok(state.outcome),
                KeyCode::Up | KeyCode::Char('k') => move_cursor(&mut state, -1),
                KeyCode::Down | KeyCode::Char('j') => move_cursor(&mut state, 1),
                KeyCode::Char(' ') => toggle(&mut state),
                KeyCode::Char('a') => set_all(&mut state, true),
                KeyCode::Char('n') => set_all(&mut state, false),
                // Apply, then stay open showing the result until the user closes.
                KeyCode::Enter => apply_state(&mut state),
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
    let results = apply_results(&state.entries, &state.items, true);
    let mut installed = 0;
    let mut removed = 0;
    let mut failed = 0;
    for r in &results {
        match r {
            ItemResult::Installed => installed += 1,
            ItemResult::Removed => removed += 1,
            ItemResult::Failed(_) => failed += 1,
            ItemResult::Unchanged => {}
        }
    }
    state.outcome.changed = installed > 0 || removed > 0;

    // Chat-transcript summary (used by the host once the popup closes).
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
    if failed > 0 {
        msg.push_str(&format!(" {failed} failed."));
    }
    state.outcome.message = msg;

    // In-popup banner shown above the (now read-only) list.
    let banner = if parts.is_empty() && failed == 0 {
        "Nothing to apply — no items changed.".to_string()
    } else {
        let mut b = format!("✓ {}", parts.join(", "));
        if parts.is_empty() {
            b = "✗ apply failed".to_string();
        }
        if failed > 0 {
            b.push_str(&format!(" — {failed} failed"));
        }
        b
    };

    // Rebuild rows from the same catalog entries used for this apply pass.
    // A fresh fetch here can overwrite the cache with a newer index than the
    // files just installed, making the next startup banner report an update
    // immediately after a successful apply.
    let name_results: Vec<(String, ItemResult)> = state
        .entries
        .iter()
        .map(|e| e.name.clone())
        .zip(results)
        .collect();
    let mut items = build_items(&state.entries);
    overlay_results(&mut items, &name_results);
    state.items = items;
    state.cursor = state.cursor.min(state.items.len().saturating_sub(1));
    state.result = Some(banner);
}

/// Overlay per-item status tags onto freshly rebuilt rows, matched by name.
fn overlay_results(items: &mut [Item], name_results: &[(String, ItemResult)]) {
    for item in items.iter_mut() {
        let Some((_, result)) = name_results.iter().find(|(name, _)| *name == item.name) else {
            continue;
        };
        match result {
            ItemResult::Installed => {
                item.tag = Some("installed".to_string());
                item.tag_color = GOOD;
            }
            ItemResult::Removed => {
                item.tag = Some("removed".to_string());
                item.tag_color = GOOD;
            }
            ItemResult::Failed(err) => {
                item.tag = Some("✗ failed".to_string());
                item.tag_color = BAD;
                item.desc = format!("Failed: {err}");
            }
            ItemResult::Unchanged => {}
        }
    }
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
    draw_footer(frame, chunks[2], state.result.is_some());
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
    // Result phase: reserve the top rows for a status banner, list below.
    let (banner, list_area) = if let Some(banner) = &state.result {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(2), Constraint::Min(1)])
            .split(area);
        (Some((banner, rows[0])), rows[1])
    } else {
        (None, area)
    };

    if let Some((banner, banner_area)) = banner {
        let failed = banner.contains("failed");
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                banner.clone(),
                Style::default()
                    .fg(if failed { BAD } else { GOOD })
                    .add_modifier(Modifier::BOLD),
            )))
            .block(Block::default().padding(ratatui::widgets::Padding::new(2, 0, 1, 0))),
            banner_area,
        );
    }

    let (title, hint) = if state.result.is_some() {
        (
            "Result",
            "Done — Enter or Esc to close. Updated items lost their \"update\" tag.",
        )
    } else {
        (
            "Tools & commands",
            "Check to install, uncheck to remove. Toggle with Space; Enter applies.",
        )
    };
    picker::draw_list(frame, list_area, title, hint, &state.items, state.cursor);
}

fn draw_footer(frame: &mut ratatui::Frame, area: Rect, applied: bool) {
    let keys: &[(&str, &str)] = if applied {
        &[("↑↓", "move"), ("enter/esc", "close")]
    } else {
        &[
            ("↑↓", "move"),
            ("space", "toggle"),
            ("a/n", "all/none"),
            ("enter", "apply"),
            ("esc", "close"),
        ]
    };
    picker::draw_footer(frame, area, keys);
}

//! `/catalog` — a fullscreen popup for browsing, installing, and removing the
//! optional tools, commands, and themes hosted in the catalog. Rows are grouped
//! into Updates, Installed, and Available sections. Installed items are checked
//! labeled; applying only acts on items the user explicitly toggled, so
//! already-installed items are preserved unless the user unchecks them.
//!
//! The daemon supplies snapshots and applies mutations; this module owns only
//! picker state and rendering. The onboarding wizard reuses [`build_items`].

use std::io;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::ui::fullscreen::{self, FullscreenTerminal};
use crate::ui::picker::{self, Item};
use crate::ui::theme::Theme;
use bone_protocol::{
    CatalogAction, CatalogActionKind, CatalogApplyResult, CatalogItem, CatalogItemOutcome,
    CatalogItemResult, CatalogSnapshot,
};

/// Result of running the popup.
pub struct Outcome {
    /// True if any item was installed or removed.
    pub changed: bool,
    /// A one-line summary suitable for the chat transcript.
    pub message: String,
}

/// Build picker rows for the given catalog entries. Installed items are checked
/// and tagged "installed"; other items are unchecked by default. Untouched items
/// are preserved on apply.
///
/// Items whose on-disk content differs from the catalog are tagged "update" and
/// pre-checked (so a plain Enter pulls every pending update at once).
pub fn build_items(entries: &[CatalogItem], theme: &Theme) -> Vec<Item> {
    entries
        .iter()
        .map(|entry| build_item(entry, theme))
        .collect()
}

fn grouped_catalog(entries: Vec<CatalogItem>, theme: &Theme) -> (Vec<CatalogItem>, Vec<Item>) {
    let items = build_items(&entries, theme);
    group_rows(entries, items)
}

fn group_rows(entries: Vec<CatalogItem>, items: Vec<Item>) -> (Vec<CatalogItem>, Vec<Item>) {
    let mut rows: Vec<_> = entries.into_iter().zip(items).collect();
    rows.sort_by(|(left_entry, left_item), (right_entry, right_item)| {
        status_rank(left_item)
            .cmp(&status_rank(right_item))
            .then_with(|| {
                left_entry
                    .name
                    .to_lowercase()
                    .cmp(&right_entry.name.to_lowercase())
            })
    });

    let counts = [0, 1, 2].map(|rank| {
        rows.iter()
            .filter(|(_, item)| status_rank(item) == rank)
            .count()
    });
    let mut previous_rank = None;
    for (_, item) in &mut rows {
        let rank = status_rank(item);
        if previous_rank != Some(rank) {
            let label = match rank {
                0 => "Updates",
                1 => "Installed",
                _ => "Available",
            };
            item.section = Some(format!("{label} ({})", counts[rank]));
            previous_rank = Some(rank);
        }
    }

    rows.into_iter().unzip()
}

fn status_rank(item: &Item) -> usize {
    if item.tag.as_deref() == Some("update") {
        0
    } else if item.checked {
        1
    } else {
        2
    }
}

fn build_item(entry: &CatalogItem, theme: &Theme) -> Item {
    let p = &theme.palette;
    let mut item = Item::new(
        entry.name.clone(),
        entry.description.clone(),
        entry.installed,
    );
    item.category = match entry.kind.as_str() {
        "command" => "command",
        "theme" => "theme",
        _ => "tool",
    };
    add_detail(&mut item, "Version", entry.version.as_deref());
    add_detail(&mut item, "Updated", entry.updated_at.as_deref());
    add_detail(&mut item, "Author", entry.author.as_deref());
    add_detail(&mut item, "Repository", entry.repository.as_deref());
    add_detail(&mut item, "Documentation", entry.documentation.as_deref());
    add_detail(
        &mut item,
        "Requires Bone",
        entry.min_bone_version.as_deref(),
    );
    if !entry.dependencies.is_empty() {
        item.details
            .push(("Dependencies".to_string(), entry.dependencies.join(", ")));
    }
    if !entry.permissions.is_empty() {
        item.details
            .push(("Permissions".to_string(), entry.permissions.join(", ")));
    }
    item.long_desc = entry
        .long_description
        .clone()
        .filter(|value| !value.is_empty());
    if entry.update_available {
        item.tag = Some("update".to_string());
        item.tag_color = Some(p.accent);
        item.user_touched = true;
    } else if entry.installed {
        item.tag = Some("installed".to_string());
        item.tag_color = Some(p.good);
    }
    item
}

fn add_detail(item: &mut Item, label: &str, value: Option<&str>) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        item.details.push((label.to_string(), value.to_string()));
    }
}

/// Turn the changed picker rows into daemon-host actions.
pub fn actions(entries: &[CatalogItem], items: &[Item], touched_only: bool) -> Vec<CatalogAction> {
    entries
        .iter()
        .zip(items.iter())
        .filter(|(entry, item)| {
            (!touched_only || item.user_touched)
                && (item.checked != entry.installed || item.checked && entry.update_available)
        })
        .map(|(entry, item)| CatalogAction {
            name: entry.name.clone(),
            action: if item.checked {
                CatalogActionKind::Install
            } else {
                CatalogActionKind::Remove
            },
        })
        .collect()
}

struct State {
    revision: String,
    entries: Vec<CatalogItem>,
    items: Vec<Item>,
    cursor: usize,
    outcome: Outcome,
    /// Set once the user has applied changes: a one-line banner summarizing what
    /// happened. `Some` switches the screen into its read-only "result" phase.
    result: Option<String>,
}

impl State {
    fn new(snapshot: CatalogSnapshot, theme: &Theme) -> Self {
        let (entries, items) = grouped_catalog(snapshot.items, theme);
        Self {
            revision: snapshot.revision,
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

/// Run the catalog popup against a daemon-host apply callback.
pub fn run<F>(theme: &Theme, snapshot: CatalogSnapshot, apply: F) -> io::Result<Outcome>
where
    F: FnMut(String, Vec<CatalogAction>) -> Result<CatalogApplyResult, String>,
{
    fullscreen::run(|term| run_loop(term, theme, snapshot, apply))
}

fn run_loop<F>(
    term: &mut FullscreenTerminal,
    theme: &Theme,
    snapshot: CatalogSnapshot,
    mut apply: F,
) -> io::Result<Outcome>
where
    F: FnMut(String, Vec<CatalogAction>) -> Result<CatalogApplyResult, String>,
{
    let mut state = State::new(snapshot, theme);
    term.draw(|frame| draw(frame, &state, theme))?;

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
                KeyCode::Enter => apply_state(&mut state, theme, &mut apply),
                _ => {}
            },
            Event::Resize(_, _) => {}
            _ => {}
        }
        term.draw(|frame| draw(frame, &state, theme))?;
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

fn apply_state<F>(state: &mut State, theme: &Theme, apply: &mut F)
where
    F: FnMut(String, Vec<CatalogAction>) -> Result<CatalogApplyResult, String>,
{
    let requested = actions(&state.entries, &state.items, true);
    let applied = match apply(state.revision.clone(), requested) {
        Ok(applied) => applied,
        Err(error) => {
            state.outcome.message = format!("Catalog failed: {error}");
            state.result = Some(format!("✗ {error}"));
            return;
        }
    };
    let mut installed = 0;
    let mut removed = 0;
    let mut failed = 0;
    for result in &applied.results {
        match &result.outcome {
            CatalogItemOutcome::Installed => installed += 1,
            CatalogItemOutcome::Removed => removed += 1,
            CatalogItemOutcome::Failed { .. } => failed += 1,
            CatalogItemOutcome::Unchanged => {}
        }
    }
    state.outcome.changed |= applied.changed;

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

    state.revision = applied.snapshot.revision;
    let (entries, mut items) = grouped_catalog(applied.snapshot.items, theme);
    overlay_results(&mut items, &applied.results, theme);
    state.entries = entries;
    state.items = items;
    state.cursor = state.cursor.min(state.items.len().saturating_sub(1));
    state.result = Some(banner);
}

/// Overlay per-item status tags onto freshly rebuilt rows, matched by name.
fn overlay_results(items: &mut [Item], results: &[CatalogItemResult], theme: &Theme) {
    let p = &theme.palette;
    for item in items.iter_mut() {
        let Some(result) = results.iter().find(|result| result.name == item.name) else {
            continue;
        };
        match &result.outcome {
            CatalogItemOutcome::Installed => {
                item.tag = Some("installed".to_string());
                item.tag_color = Some(p.good);
            }
            CatalogItemOutcome::Removed => {
                item.tag = Some("removed".to_string());
                item.tag_color = Some(p.good);
            }
            CatalogItemOutcome::Failed { message } => {
                item.tag = Some("✗ failed".to_string());
                item.tag_color = Some(p.error);
                item.desc = format!("Failed: {message}");
            }
            CatalogItemOutcome::Unchanged => {}
        }
    }
}

// ---- rendering ----------------------------------------------------------

fn draw(frame: &mut ratatui::Frame, state: &State, theme: &Theme) {
    let p = &theme.palette;
    let screen = frame.area();
    frame.render_widget(
        Block::default().style(match p.bg {
            Some(bg) => Style::default().bg(bg),
            None => Style::default(),
        }),
        screen,
    );

    let area = screen;

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header
            Constraint::Min(1),    // body
            Constraint::Length(1), // footer
        ])
        .split(area);

    draw_header(frame, chunks[0], theme);
    draw_body(frame, chunks[1], state, theme);
    draw_footer(frame, chunks[2], state.result.is_some(), theme);
}

fn draw_header(frame: &mut ratatui::Frame, area: Rect, theme: &Theme) {
    let p = &theme.palette;
    let lines = vec![
        Line::from(vec![
            Span::styled(
                "bone ",
                Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled("catalog", Style::default().fg(p.muted)),
        ]),
        Line::from(Span::styled(
            "Optional tools, commands & themes — download on demand",
            Style::default().fg(p.subtle),
        )),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(p.border))
                .padding(ratatui::widgets::Padding::new(2, 0, 1, 0)),
        ),
        area,
    );
}

fn draw_body(frame: &mut ratatui::Frame, area: Rect, state: &State, theme: &Theme) {
    let p = &theme.palette;
    if state.items.is_empty() {
        let lines = vec![
            Line::from(Span::styled(
                "No catalog items available.",
                Style::default().fg(p.fg).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "bone couldn't reach the catalog (you may be offline). Anything \
                 already installed still works; try again later.",
                Style::default().fg(p.muted),
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
                    .fg(if failed { p.error } else { p.good })
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
    picker::draw_list(
        frame,
        list_area,
        title,
        hint,
        &state.items,
        state.cursor,
        theme,
    );
}

fn draw_footer(frame: &mut ratatui::Frame, area: Rect, applied: bool, theme: &Theme) {
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
    picker::draw_footer(frame, area, keys, theme);
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;

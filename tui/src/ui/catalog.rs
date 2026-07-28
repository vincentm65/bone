//! `/catalog` — a fullscreen popup for browsing, installing, and removing the
//! optional tools, commands, and themes hosted in the catalog. Rows are grouped
//! into Updates, Installed, and Available sections. Installed items are checked
//! labeled; applying only acts on items the user explicitly toggled, so
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
use crate::ui::picker::{self, Item};
use crate::ui::theme::Theme;

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
pub fn build_items(entries: &[CatalogEntry], theme: &Theme) -> Vec<Item> {
    entries
        .iter()
        .map(|entry| {
            let installed = catalog::is_installed(entry);
            build_item(
                entry,
                installed,
                installed && catalog::needs_update(entry),
                theme,
            )
        })
        .collect()
}

fn grouped_catalog(entries: Vec<CatalogEntry>, theme: &Theme) -> (Vec<CatalogEntry>, Vec<Item>) {
    let items = build_items(&entries, theme);
    group_rows(entries, items)
}

fn group_rows(entries: Vec<CatalogEntry>, items: Vec<Item>) -> (Vec<CatalogEntry>, Vec<Item>) {
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

fn build_item(entry: &CatalogEntry, installed: bool, update: bool, theme: &Theme) -> Item {
    let p = &theme.palette;
    let mut item = Item::new(entry.name.clone(), entry.description.clone(), installed);
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
    if update {
        item.tag = Some("update".to_string());
        item.tag_color = Some(p.accent);
        item.user_touched = true;
    } else if installed {
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

/// Install or remove one catalog item by its display name (`weather`) or file
/// name (`weather.lua`).
pub fn apply_named(action: &str, name: &str) -> Outcome {
    let entries = catalog::sync_quiet();
    let requested = name.strip_suffix(".lua").unwrap_or(name);
    let Some(entry) = entries
        .iter()
        .find(|entry| entry.name.strip_suffix(".lua") == Some(requested))
    else {
        return Outcome {
            changed: false,
            message: format!("Catalog item not found: {name}"),
        };
    };

    let result = match action {
        "install" if catalog::is_installed(entry) && !catalog::needs_update(entry) => {
            return Outcome {
                changed: false,
                message: format!("Catalog item already installed: {requested}"),
            };
        }
        "install" => catalog::install(entry),
        "remove" if !catalog::has_installed_files(entry) => {
            return Outcome {
                changed: false,
                message: format!("Catalog item is not installed: {requested}"),
            };
        }
        "remove" => catalog::remove(entry),
        _ => {
            return Outcome {
                changed: false,
                message: "Usage: /catalog install|remove NAME".to_string(),
            };
        }
    };

    match result {
        Ok(()) => Outcome {
            changed: true,
            message: format!(
                "Catalog item {}: {requested}",
                if action == "install" {
                    "installed"
                } else {
                    "removed"
                }
            ),
        },
        Err(err) => Outcome {
            changed: false,
            message: format!("Catalog {action} failed for {requested}: {err}"),
        },
    }
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
    fn new(theme: &Theme) -> Self {
        let entries = catalog::sync_quiet();
        let (entries, items) = grouped_catalog(entries, theme);
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
pub fn run(theme: &Theme) -> io::Result<Outcome> {
    fullscreen::run(|term| run_loop(term, theme))
}

fn run_loop(term: &mut FullscreenTerminal, theme: &Theme) -> io::Result<Outcome> {
    let mut state = State::new(theme);
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
                KeyCode::Enter => apply_state(&mut state, theme),
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

fn apply_state(state: &mut State, theme: &Theme) {
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
    let (entries, mut items) = grouped_catalog(std::mem::take(&mut state.entries), theme);
    overlay_results(&mut items, &name_results, theme);
    state.entries = entries;
    state.items = items;
    state.cursor = state.cursor.min(state.items.len().saturating_sub(1));
    state.result = Some(banner);
}

/// Overlay per-item status tags onto freshly rebuilt rows, matched by name.
fn overlay_results(items: &mut [Item], name_results: &[(String, ItemResult)], theme: &Theme) {
    let p = &theme.palette;
    for item in items.iter_mut() {
        let Some((_, result)) = name_results.iter().find(|(name, _)| *name == item.name) else {
            continue;
        };
        match result {
            ItemResult::Installed => {
                item.tag = Some("installed".to_string());
                item.tag_color = Some(p.good);
            }
            ItemResult::Removed => {
                item.tag = Some("removed".to_string());
                item.tag_color = Some(p.good);
            }
            ItemResult::Failed(err) => {
                item.tag = Some("✗ failed".to_string());
                item.tag_color = Some(p.error);
                item.desc = format!("Failed: {err}");
            }
            ItemResult::Unchanged => {}
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
mod tests {
    use super::*;

    fn entry(kind: &str) -> CatalogEntry {
        CatalogEntry {
            name: "demo.lua".to_string(),
            kind: kind.to_string(),
            description: "Demo extension".to_string(),
            ..CatalogEntry::default()
        }
    }

    fn make_theme() -> Theme {
        Theme::default()
    }

    fn result_state(banner: &str) -> State {
        State {
            entries: Vec::new(),
            items: vec![Item::new("demo.lua".into(), "Demo".into(), false)],
            cursor: 0,
            outcome: Outcome {
                changed: false,
                message: String::new(),
            },
            result: Some(banner.to_string()),
        }
    }

    #[test]
    fn result_overlays_use_good_and_error_roles() {
        let mut theme = make_theme();
        theme.palette.good = ratatui::style::Color::Rgb(1, 2, 3);
        theme.palette.error = ratatui::style::Color::Rgb(4, 5, 6);
        let mut items = vec![
            Item::new("installed".into(), String::new(), false),
            Item::new("removed".into(), String::new(), false),
            Item::new("failed".into(), String::new(), false),
            Item::new("unchanged".into(), String::new(), false),
        ];
        items[3].tag = Some("keep".into());
        let results = vec![
            ("installed".into(), ItemResult::Installed),
            ("removed".into(), ItemResult::Removed),
            ("failed".into(), ItemResult::Failed("network".into())),
            ("unchanged".into(), ItemResult::Unchanged),
        ];

        overlay_results(&mut items, &results, &theme);

        assert_eq!(items[0].tag_color, Some(theme.palette.good));
        assert_eq!(items[1].tag_color, Some(theme.palette.good));
        assert_eq!(items[2].tag_color, Some(theme.palette.error));
        assert_eq!(items[2].desc, "Failed: network");
        assert_eq!(items[3].tag.as_deref(), Some("keep"));
        assert_eq!(items[3].tag_color, None);
    }

    #[test]
    fn result_banners_render_with_success_and_error_roles() {
        let mut theme = make_theme();
        theme.palette.good = ratatui::style::Color::Rgb(7, 8, 9);
        theme.palette.error = ratatui::style::Color::Rgb(10, 11, 12);

        for (banner, expected) in [
            ("✓ installed 1", theme.palette.good),
            ("✗ 1 failed", theme.palette.error),
        ] {
            let state = result_state(banner);
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 14)).unwrap();
            terminal
                .draw(|frame| draw_body(frame, frame.area(), &state, &theme))
                .unwrap();
            assert_eq!(
                terminal.backend().buffer().cell((2, 1)).unwrap().fg,
                expected
            );
        }
    }

    #[test]
    fn available_item_is_unchecked_without_status() {
        let theme = make_theme();
        let item = build_item(&entry("tool"), false, false, &theme);

        assert!(!item.checked);
        assert!(!item.user_touched);
        assert_eq!(item.tag, None);
        assert_eq!(item.category, "tool");
    }

    #[test]
    fn installed_item_is_checked_and_labeled() {
        let theme = make_theme();
        let item = build_item(&entry("command"), true, false, &theme);

        assert!(item.checked);
        assert!(!item.user_touched);
        assert_eq!(item.tag.as_deref(), Some("installed"));
        assert_eq!(item.tag_color, Some(theme.palette.good));
        assert_eq!(item.category, "command");
    }

    #[test]
    fn theme_item_has_distinct_category() {
        let theme = make_theme();
        let item = build_item(&entry("theme"), false, false, &theme);

        assert_eq!(item.category, "theme");
    }

    #[test]
    fn pending_update_takes_precedence_and_is_applied_by_default() {
        let theme = make_theme();
        let item = build_item(&entry("tool"), true, true, &theme);

        assert!(item.checked);
        assert!(item.user_touched);
        assert_eq!(item.tag.as_deref(), Some("update"));
        assert_eq!(item.tag_color, Some(theme.palette.accent));
    }

    #[test]
    fn metadata_is_added_to_the_detail_pane() {
        let mut entry = entry("tool");
        entry.version = Some("1.2.3".to_string());
        entry.updated_at = Some("2026-03-10".to_string());
        entry.author = Some("Bone Team".to_string());
        entry.repository = Some("https://example.com/repo".to_string());
        entry.documentation = Some("https://example.com/docs".to_string());
        entry.min_bone_version = Some(">=2.4".to_string());
        entry.dependencies = vec!["helper.lua".to_string()];
        entry.permissions = vec!["network".to_string(), "filesystem".to_string()];
        entry.long_description = Some("A longer explanation.".to_string());

        let theme = make_theme();
        let item = build_item(&entry, false, false, &theme);

        assert_eq!(item.details.len(), 8);
        assert_eq!(
            item.details[0],
            ("Version".to_string(), "1.2.3".to_string())
        );
        assert_eq!(item.details[7].0, "Permissions");
        assert_eq!(item.long_desc.as_deref(), Some("A longer explanation."));
    }

    #[test]
    fn rows_are_grouped_as_updates_installed_and_available() {
        let theme = make_theme();
        let entries = vec![
            CatalogEntry {
                name: "available.lua".to_string(),
                ..entry("tool")
            },
            CatalogEntry {
                name: "update.lua".to_string(),
                ..entry("tool")
            },
            CatalogEntry {
                name: "installed.lua".to_string(),
                ..entry("command")
            },
        ];
        let items = vec![
            build_item(&entries[0], false, false, &theme),
            build_item(&entries[1], true, true, &theme),
            build_item(&entries[2], true, false, &theme),
        ];

        let (entries, items) = group_rows(entries, items);

        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["update.lua", "installed.lua", "available.lua"]
        );
        assert_eq!(items[0].section.as_deref(), Some("Updates (1)"));
        assert_eq!(items[1].section.as_deref(), Some("Installed (1)"));
        assert_eq!(items[2].section.as_deref(), Some("Available (1)"));

        let width = 100;
        let height = 20;
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                picker::draw_list(
                    frame,
                    frame.area(),
                    "Catalog",
                    "Grouped extensions",
                    &items,
                    0,
                    &theme,
                );
            })
            .unwrap();
        let screen = (0..height)
            .map(|row| {
                (0..width)
                    .map(|column| {
                        terminal
                            .backend()
                            .buffer()
                            .cell((column, row))
                            .unwrap()
                            .symbol()
                    })
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(screen.contains("Updates (1)"), "{screen}");
        assert!(screen.contains("Installed (1)"), "{screen}");
        assert!(screen.contains("Available (1)"), "{screen}");
    }

    #[test]
    fn theme_render_assertion() {
        let theme = Theme::default();
        let item = build_item(&entry("tool"), true, false, &theme);
        assert_eq!(item.tag.as_deref(), Some("installed"));
        assert_eq!(item.tag_color, Some(theme.palette.good));
    }
}

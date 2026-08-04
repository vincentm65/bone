//! Shared selection, navigation, and layout mechanics for native list panes.

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::input::InputState;
use super::pane_page::PanePage;
use crate::ui::theme::Theme;

pub(crate) const VISIBLE_ROWS: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SelectablePaneAction {
    Unhandled,
    InputChanged,
    SelectionChanged,
    Open(String),
    Cancel(String),
}

pub(crate) fn apply_nav_key(
    code: KeyCode,
    modifiers: KeyModifiers,
    active_ids: &[String],
    selected_id: &mut Option<String>,
    allow_open: bool,
) -> SelectablePaneAction {
    if !modifiers.is_empty() || active_ids.is_empty() {
        return SelectablePaneAction::Unhandled;
    }
    let selected_index = selected_id
        .as_deref()
        .and_then(|id| active_ids.iter().position(|active| active == id));
    let current = selected_index.unwrap_or(0);
    match code {
        KeyCode::Up => {
            *selected_id = Some(active_ids[current.saturating_sub(1)].clone());
            SelectablePaneAction::SelectionChanged
        }
        KeyCode::Down => {
            *selected_id = Some(active_ids[(current + 1).min(active_ids.len() - 1)].clone());
            SelectablePaneAction::SelectionChanged
        }
        KeyCode::Enter if allow_open => selected_index
            .map(|index| SelectablePaneAction::Open(active_ids[index].clone()))
            .unwrap_or(SelectablePaneAction::Unhandled),
        KeyCode::Char('k') => selected_index
            .map(|index| SelectablePaneAction::Cancel(active_ids[index].clone()))
            .unwrap_or(SelectablePaneAction::Unhandled),
        _ => SelectablePaneAction::Unhandled,
    }
}

/// Treat submitted inputs and agent rows as one vertical navigation sequence.
/// The input sits below the pane: Up walks history before entering the pane at
/// its bottom row; Down leaves the bottom row through history back to live input.
pub(crate) fn apply_agent_nav_key(
    code: KeyCode,
    modifiers: KeyModifiers,
    active_ids: &[String],
    selected_id: &mut Option<String>,
    input: &mut InputState,
    pane_focused: &mut bool,
    allow_open: bool,
) -> SelectablePaneAction {
    if !modifiers.is_empty() {
        return SelectablePaneAction::Unhandled;
    }

    match code {
        KeyCode::Up if !*pane_focused => {
            if input.history_up() {
                SelectablePaneAction::InputChanged
            } else if input.history_index.is_none() && !input.buffer.is_empty() {
                SelectablePaneAction::Unhandled
            } else if let Some(last) = active_ids.last() {
                input.select_live_input();
                *pane_focused = true;
                *selected_id = Some(last.clone());
                SelectablePaneAction::SelectionChanged
            } else {
                SelectablePaneAction::Unhandled
            }
        }
        KeyCode::Down if !*pane_focused => {
            if input.history_down() {
                SelectablePaneAction::InputChanged
            } else {
                SelectablePaneAction::Unhandled
            }
        }
        KeyCode::Down if !active_ids.is_empty() => {
            let current = selected_id
                .as_deref()
                .and_then(|id| active_ids.iter().position(|active| active == id))
                .unwrap_or(active_ids.len() - 1);
            if current + 1 < active_ids.len() {
                *selected_id = Some(active_ids[current + 1].clone());
                SelectablePaneAction::SelectionChanged
            } else {
                *pane_focused = false;
                input.select_oldest_history();
                SelectablePaneAction::InputChanged
            }
        }
        _ => apply_nav_key(code, modifiers, active_ids, selected_id, allow_open),
    }
}

pub(crate) fn reconcile_selection(selected_id: &mut Option<String>, active_ids: &[String]) {
    if !selected_id
        .as_ref()
        .is_some_and(|selected| active_ids.contains(selected))
    {
        *selected_id = active_ids.first().cloned();
    }
}

pub(crate) fn render(
    theme: &Theme,
    source: &str,
    title: String,
    rows: Vec<(bool, Line<'static>)>,
) -> PanePage {
    let selected_index = rows.iter().position(|(selected, _)| *selected).unwrap_or(0);
    let content = rows
        .into_iter()
        .map(|(selected, mut line)| {
            line.spans.insert(
                0,
                Span::styled(
                    if selected { " › " } else { "   " },
                    Style::default().fg(if selected {
                        theme.palette.accent
                    } else {
                        theme.palette.muted
                    }),
                ),
            );
            if selected {
                line = line.style(Style::default().bg(theme.palette.selection));
            }
            line
        })
        .collect();

    PanePage {
        source: source.into(),
        title,
        content,
        visible_rows: VISIBLE_ROWS,
        scroll: selected_index.saturating_sub(VISIBLE_ROWS.saturating_sub(1)),
    }
}

#[cfg(test)]
#[path = "selectable_pane_tests.rs"]
mod tests;

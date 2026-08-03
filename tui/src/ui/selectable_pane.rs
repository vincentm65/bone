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
mod tests {
    use super::*;

    #[test]
    fn navigation_moves_and_clamps() {
        let ids = vec!["first".into(), "second".into()];
        let mut selected = Some("first".into());

        assert_eq!(
            apply_nav_key(KeyCode::Down, KeyModifiers::NONE, &ids, &mut selected, true,),
            SelectablePaneAction::SelectionChanged
        );
        assert_eq!(selected.as_deref(), Some("second"));
        apply_nav_key(KeyCode::Down, KeyModifiers::NONE, &ids, &mut selected, true);
        assert_eq!(selected.as_deref(), Some("second"));
        apply_nav_key(KeyCode::Up, KeyModifiers::NONE, &ids, &mut selected, true);
        assert_eq!(selected.as_deref(), Some("first"));
    }

    #[test]
    fn open_cancel_and_fallthrough_require_valid_unmodified_selection() {
        let ids = vec!["item".into()];
        let mut selected = Some("item".into());

        assert_eq!(
            apply_nav_key(
                KeyCode::Enter,
                KeyModifiers::NONE,
                &ids,
                &mut selected,
                true,
            ),
            SelectablePaneAction::Open("item".into())
        );
        assert_eq!(
            apply_nav_key(
                KeyCode::Char('k'),
                KeyModifiers::NONE,
                &ids,
                &mut selected,
                true,
            ),
            SelectablePaneAction::Cancel("item".into())
        );
        assert_eq!(
            apply_nav_key(
                KeyCode::Enter,
                KeyModifiers::NONE,
                &ids,
                &mut selected,
                false,
            ),
            SelectablePaneAction::Unhandled
        );
        assert_eq!(
            apply_nav_key(
                KeyCode::Down,
                KeyModifiers::SHIFT,
                &ids,
                &mut selected,
                true,
            ),
            SelectablePaneAction::Unhandled
        );

        selected = Some("stale".into());
        for code in [KeyCode::Enter, KeyCode::Char('k')] {
            assert_eq!(
                apply_nav_key(code, KeyModifiers::NONE, &ids, &mut selected, true),
                SelectablePaneAction::Unhandled
            );
        }
    }

    #[test]
    fn empty_navigation_is_unhandled() {
        let mut selected = None;
        for code in [
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Enter,
            KeyCode::Char('k'),
        ] {
            assert_eq!(
                apply_nav_key(code, KeyModifiers::NONE, &[], &mut selected, true),
                SelectablePaneAction::Unhandled
            );
        }
    }

    #[test]
    fn agent_navigation_continues_through_input_history_in_both_directions() {
        let ids = vec!["agent-top".into(), "agent-bottom".into()];
        let mut selected = Some("agent-top".into());
        let mut input = InputState::default();
        input.buffer = "oldest input".into();
        input.reset();
        input.buffer = "newest input".into();
        input.reset();
        let mut pane_focused = false;

        for expected in ["newest input", "oldest input"] {
            assert_eq!(
                apply_agent_nav_key(
                    KeyCode::Up,
                    KeyModifiers::NONE,
                    &ids,
                    &mut selected,
                    &mut input,
                    &mut pane_focused,
                    true,
                ),
                SelectablePaneAction::InputChanged
            );
            assert_eq!(input.buffer, expected);
        }

        assert_eq!(
            apply_agent_nav_key(
                KeyCode::Up,
                KeyModifiers::NONE,
                &ids,
                &mut selected,
                &mut input,
                &mut pane_focused,
                true,
            ),
            SelectablePaneAction::SelectionChanged
        );
        assert!(pane_focused);
        assert_eq!(selected.as_deref(), Some("agent-bottom"));
        assert!(input.buffer.is_empty());
        assert!(input.history_index.is_none());

        apply_agent_nav_key(
            KeyCode::Up,
            KeyModifiers::NONE,
            &ids,
            &mut selected,
            &mut input,
            &mut pane_focused,
            true,
        );
        assert_eq!(selected.as_deref(), Some("agent-top"));
        apply_agent_nav_key(
            KeyCode::Down,
            KeyModifiers::NONE,
            &ids,
            &mut selected,
            &mut input,
            &mut pane_focused,
            true,
        );
        assert_eq!(selected.as_deref(), Some("agent-bottom"));

        assert_eq!(
            apply_agent_nav_key(
                KeyCode::Down,
                KeyModifiers::NONE,
                &ids,
                &mut selected,
                &mut input,
                &mut pane_focused,
                true,
            ),
            SelectablePaneAction::InputChanged
        );
        assert!(!pane_focused);
        assert_eq!(input.buffer, "oldest input");
        assert!(input.history_down());
        assert_eq!(input.buffer, "newest input");
        assert!(input.history_down());
        assert!(input.buffer.is_empty());
    }

    #[test]
    fn agent_navigation_preserves_live_input_without_history() {
        let ids = vec!["agent".into()];
        let mut selected = Some("agent".into());
        let mut input = InputState {
            buffer: "unsent draft".into(),
            cursor_pos: 12,
            ..Default::default()
        };
        let mut pane_focused = false;

        assert_eq!(
            apply_agent_nav_key(
                KeyCode::Up,
                KeyModifiers::NONE,
                &ids,
                &mut selected,
                &mut input,
                &mut pane_focused,
                true,
            ),
            SelectablePaneAction::Unhandled
        );
        assert_eq!(input.buffer, "unsent draft");
        assert!(!pane_focused);
    }

    #[test]
    fn reconciliation_preserves_valid_selection_and_repairs_stale_selection() {
        let ids = vec!["first".into(), "second".into()];
        let mut selected = Some("second".into());
        reconcile_selection(&mut selected, &ids);
        assert_eq!(selected.as_deref(), Some("second"));

        selected = Some("stale".into());
        reconcile_selection(&mut selected, &ids);
        assert_eq!(selected.as_deref(), Some("first"));

        reconcile_selection(&mut selected, &[]);
        assert_eq!(selected, None);
    }

    #[test]
    fn render_marks_selection_and_scrolls_it_into_view() {
        let rows = (0..10)
            .map(|index| (index == 9, Line::raw(format!("row {index}"))))
            .collect();
        let mut theme = Theme::default();
        theme.palette.selection = ratatui::style::Color::Blue;
        let page = render(&theme, "test", "Test".into(), rows);

        assert_eq!(page.visible_rows, 8);
        assert_eq!(page.scroll, 2);
        assert!(page.content[9].to_string().contains('›'));
        assert_eq!(page.content[9].style.bg, Some(theme.palette.selection));
    }
}

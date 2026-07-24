//! Shared selection, navigation, and layout mechanics for native list panes.

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use super::pane_page::PanePage;

pub(crate) const VISIBLE_ROWS: usize = 8;
const SELECTED_BG: Color = Color::Rgb(0x3A, 0x3F, 0x4B);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SelectablePaneAction {
    Unhandled,
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

pub(crate) fn reconcile_selection(selected_id: &mut Option<String>, active_ids: &[String]) {
    if !selected_id
        .as_ref()
        .is_some_and(|selected| active_ids.contains(selected))
    {
        *selected_id = active_ids.first().cloned();
    }
}

pub(crate) fn render(source: &str, title: String, rows: Vec<(bool, Line<'static>)>) -> PanePage {
    let selected_index = rows.iter().position(|(selected, _)| *selected).unwrap_or(0);
    let content = rows
        .into_iter()
        .map(|(selected, mut line)| {
            line.spans.insert(
                0,
                Span::styled(
                    if selected { " › " } else { "   " },
                    Style::default().fg(if selected {
                        Color::White
                    } else {
                        Color::DarkGray
                    }),
                ),
            );
            if selected {
                line = line.style(Style::default().bg(SELECTED_BG));
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
        let page = render("test", "Test".into(), rows);

        assert_eq!(page.visible_rows, 8);
        assert_eq!(page.scroll, 2);
        assert!(page.content[9].to_string().contains('›'));
        assert_eq!(page.content[9].style.bg, Some(SELECTED_BG));
    }
}

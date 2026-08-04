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

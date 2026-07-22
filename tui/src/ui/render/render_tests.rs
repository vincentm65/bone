use super::*;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;

#[test]
fn max_viewport_height_reserves_a_row_when_possible() {
    assert_eq!(max_viewport_height(0), 1);
    assert_eq!(max_viewport_height(1), 1);
    assert_eq!(max_viewport_height(2), 1);
    assert_eq!(max_viewport_height(3), 2);
    assert_eq!(max_viewport_height(24), 23);
    assert_eq!(max_viewport_height(u16::MAX), u16::MAX - 1);
}

#[test]
fn initial_viewport_height_clamps_minimum_rows_to_terminal() {
    assert_eq!(initial_viewport_height(0), 1);
    assert_eq!(initial_viewport_height(1), 1);
    assert_eq!(initial_viewport_height(2), 1);
    assert_eq!(initial_viewport_height(3), 2);
    assert_eq!(initial_viewport_height(4), MIN_ROWS);
    assert_eq!(initial_viewport_height(24), MIN_ROWS);
}

#[test]
fn desired_viewport_height_tracks_input_panes_completion_and_running_rows() {
    let renderer = Renderer::new();
    let mut input = crate::ui::input::InputState::default();
    let empty = renderer.desired_height(&input, None, 40, &[], 0, None, 0);

    input.buffer = "first\nsecond\nthird".into();
    input.cursor_pos = input.buffer.chars().count();
    let multiline = renderer.desired_height(&input, None, 40, &[], 0, None, 0);
    assert!(multiline > empty);

    input.reset();
    assert_eq!(
        renderer.desired_height(&input, None, 40, &[], 0, None, 0),
        empty
    );

    let page = crate::ui::pane_page::PanePage {
        source: "test".into(),
        title: "test".into(),
        content: vec![Line::raw("one"), Line::raw("two")],
        visible_rows: 2,
        scroll: 0,
    };
    let pane_open = renderer.desired_height(&input, None, 40, &[page], 0, None, 0);
    assert!(pane_open > empty);

    let completion = crate::ui::autocomplete::AutocompleteState::new(vec![(
        "command".into(),
        "description".into(),
    )]);
    let completion_open = renderer.desired_height(&input, None, 40, &[], 0, Some(&completion), 0);
    assert_eq!(completion_open, empty + completion.visible_rows());

    let running = renderer.desired_height(&input, None, 40, &[], 0, None, 2);
    assert_eq!(running, empty + 2);
}

#[test]
fn consecutive_scrollback_separators_are_deduplicated() {
    let mut renderer = Renderer::new();
    let blank = [Line::raw("")];

    assert_eq!(renderer.dedup_scrollback_blanks(&blank).len(), 1);
    assert!(renderer.dedup_scrollback_blanks(&blank).is_empty());

    let content = [Line::raw("next")];
    assert_eq!(renderer.dedup_scrollback_blanks(&content).len(), 1);
    assert_eq!(renderer.dedup_scrollback_blanks(&blank).len(), 1);
}

#[test]
fn terminal_color_rgb_maps_truecolor_and_named_colors() {
    assert_eq!(terminal_color_rgb(Color::Rgb(1, 2, 3)), (1, 2, 3));
    assert_eq!(terminal_color_rgb(Color::Black), (0, 0, 0));
    assert_eq!(terminal_color_rgb(Color::White), (255, 255, 255));
    assert_eq!(terminal_color_rgb(Color::LightBlue), (0x3B, 0x8E, 0xEA));
}

/// Reproduces the panic shape from counting wrap height at a wider width than
/// the `insert_before` temp buffer (viewport) actually has.
#[test]
fn render_scrollback_lines_survives_underallocated_height() {
    // Content that wraps to more rows at width 99 than at width 100.
    let lines = vec![
        Line::from("x".repeat(100)),
        Line::from("hello"),
        Line::from("z".repeat(150)),
    ];
    // Pre-fix row count at the *wrong* (wider) width — this is what the
    // old `term.size().width` path did when the viewport lagged a Resize.
    let under_height = logical_lines_row_count(&lines, 100);
    let correct_height = logical_lines_row_count(&lines, 99);
    assert!(
        correct_height > under_height,
        "fixture must need more rows at the narrower width ({correct_height} > {under_height})"
    );

    let area = Rect {
        x: 0,
        y: 0,
        width: 99,
        height: under_height,
    };
    let mut buf = Buffer::empty(area);
    // Must not panic even though the allocated height is too short for the
    // true wrap at buf width — the renderer clamps to remaining rows.
    render_scrollback_lines(&lines, &mut buf);
}

#[test]
fn logical_lines_row_count_uses_wrap_width() {
    let line = Line::from("x".repeat(100));
    assert_eq!(logical_lines_row_count(std::slice::from_ref(&line), 100), 1);
    assert_eq!(logical_lines_row_count(&[line], 99), 2);
}

use super::*;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;

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
    assert_eq!(logical_lines_row_count(&[line.clone()], 100), 1);
    assert_eq!(logical_lines_row_count(&[line], 99), 2);
}

use super::*;

#[test]
fn list_renderer_uses_semantic_roles_and_preserves_explicit_tag_color() {
    let mut theme = Theme::default();
    theme.palette.fg = Color::Rgb(1, 2, 3);
    theme.palette.muted = Color::Rgb(4, 5, 6);
    theme.palette.subtle = Color::Rgb(7, 8, 9);
    theme.palette.accent = Color::Rgb(10, 11, 12);
    theme.palette.good = Color::Rgb(13, 14, 15);
    theme.palette.border = Color::Rgb(16, 17, 18);
    let tag_color = Color::Rgb(19, 20, 21);

    let mut selected = Item::new("selected.lua".into(), "Summary".into(), true);
    selected.category = "tool";
    selected.tag = Some("custom".into());
    selected.tag_color = Some(tag_color);
    selected.section = Some("Section".into());
    selected.details.push(("Version".into(), "1.0".into()));
    selected.long_desc = Some("Long description".into());
    let inactive = Item::new("inactive.lua".into(), String::new(), false);

    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 16)).unwrap();
    terminal
        .draw(|frame| {
            draw_list(
                frame,
                frame.area(),
                "Picker title",
                "Picker hint",
                &[selected, inactive],
                0,
                &theme,
            );
        })
        .unwrap();
    let buffer = terminal.backend().buffer();

    assert_eq!(buffer.cell((2, 1)).unwrap().fg, theme.palette.fg);
    assert_eq!(buffer.cell((2, 2)).unwrap().fg, theme.palette.subtle);
    assert_eq!(buffer.cell((2, 4)).unwrap().fg, theme.palette.fg);
    assert_eq!(buffer.cell((3, 5)).unwrap().fg, theme.palette.accent);
    assert_eq!(buffer.cell((5, 5)).unwrap().fg, theme.palette.good);
    assert_eq!(buffer.cell((9, 5)).unwrap().fg, theme.palette.accent);
    assert_eq!(buffer.cell((26, 5)).unwrap().fg, tag_color);
    assert_eq!(buffer.cell((5, 6)).unwrap().fg, theme.palette.subtle);
    assert_eq!(buffer.cell((9, 6)).unwrap().fg, theme.palette.muted);
    assert_eq!(buffer.cell((31, 4)).unwrap().fg, theme.palette.border);
    assert_eq!(buffer.cell((34, 4)).unwrap().fg, theme.palette.fg);
    assert_eq!(buffer.cell((34, 6)).unwrap().fg, theme.palette.muted);
    assert_eq!(buffer.cell((34, 8)).unwrap().fg, theme.palette.subtle);
    assert_eq!(buffer.cell((43, 8)).unwrap().fg, theme.palette.fg);
    assert_eq!(buffer.cell((34, 10)).unwrap().fg, theme.palette.muted);
}

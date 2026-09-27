use super::*;

#[test]
fn renders_selected_queue_item_and_help() {
    let queue = VecDeque::from(["first".to_string(), "second\nline".to_string()]);
    let mut theme = Theme::default();
    theme.palette.accent = ratatui::style::Color::Rgb(1, 2, 3);
    theme.palette.selection = ratatui::style::Color::Rgb(4, 5, 6);
    theme.palette.fg = ratatui::style::Color::Rgb(7, 8, 9);
    theme.palette.muted = ratatui::style::Color::Rgb(10, 11, 12);
    let page = render(&queue, 1, &theme).unwrap();

    assert_eq!(page.title, "Queue (2)");
    assert_eq!(page.content.len(), 4);
    assert_eq!(page.content[0].spans[0].style.fg, Some(theme.palette.muted));
    assert_eq!(page.content[0].spans[1].style.fg, Some(theme.palette.muted));
    assert_eq!(page.content[0].spans[2].style.fg, Some(theme.palette.fg));
    assert_eq!(page.content[1].style.bg, Some(theme.palette.selection));
    assert_eq!(
        page.content[1].spans[0].style.fg,
        Some(theme.palette.accent)
    );
    assert_eq!(page.content[1].spans[1].style.fg, Some(theme.palette.muted));
    assert_eq!(page.content[1].spans[2].style.fg, Some(theme.palette.fg));
    assert!(page.content[1].to_string().contains("second line"));
    assert!(page.content[2].to_string().contains("reorder"));
    assert_eq!(page.content[2].spans[0].style.fg, Some(theme.palette.muted));
    assert!(page.content[3].to_string().contains("Ctrl/Alt+Enter"));
    assert_eq!(page.content[3].spans[0].style.fg, Some(theme.palette.muted));
}

#[test]
fn empty_queue_has_no_pane() {
    assert!(render(&VecDeque::new(), 0, &Theme::default()).is_none());
}

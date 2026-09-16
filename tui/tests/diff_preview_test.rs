use bone::chat::Message;
use bone::ui::render::messages::msg_to_lines;
use bone::ui::theme::Theme;
use ratatui::style::Color;
use ratatui::text::Line;
use unicode_width::UnicodeWidthStr;

fn line_text(line: &Line<'static>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
}

fn preview_lines(content: &str, width: u16) -> Vec<Line<'static>> {
    preview_lines_with(content, width, &Theme::default())
}

fn preview_lines_with(content: &str, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    msg_to_lines(&[Message::system(content)], theme, None, width, false)
}

#[test]
fn long_added_and_removed_diff_lines_wrap_with_blank_gutters_and_backgrounds() {
    let width = 24;
    let theme = Theme::default();
    let lines = msg_to_lines(
        &[Message::system(
            "\nfile.rs | -1 | +1\n   12 - alpha beta gamma delta epsilon\n   12 + one two three four five six",
        )],
        &theme,
        None,
        width,
        false,
    );
    let removed = lines
        .iter()
        .filter(|line| {
            line.spans.first().and_then(|span| span.style.bg) == Some(theme.diff_removed_bg)
        })
        .collect::<Vec<_>>();
    let added = lines
        .iter()
        .filter(|line| {
            line.spans.first().and_then(|span| span.style.bg) == Some(theme.diff_added_bg)
        })
        .collect::<Vec<_>>();

    assert!(removed.len() > 1);
    assert!(added.len() > 1);
    assert!(line_text(removed[0]).starts_with("     12 - "));
    assert!(line_text(added[0]).starts_with("     12 + "));
    assert!(
        removed[1..]
            .iter()
            .all(|line| line_text(line).starts_with("          "))
    );
    assert!(
        added[1..]
            .iter()
            .all(|line| line_text(line).starts_with("          "))
    );
    assert!(
        removed
            .iter()
            .all(|line| line.spans[0].style.bg == Some(theme.diff_removed_bg))
    );
    assert!(
        added
            .iter()
            .all(|line| line.spans[0].style.bg == Some(theme.diff_added_bg))
    );
    assert!(removed.iter().chain(added.iter()).all(|line| {
        UnicodeWidthStr::width(line_text(line).as_str()) == usize::from(width)
            && !line_text(line).trim().is_empty()
    }));
}

#[test]
fn indented_changed_expression_keeps_body_indent_on_continuations() {
    let lines = preview_lines(
        "\nfile.rs | -0 | +1\n   34 +             !StreamFailure::Provider(LlmError::new(kind, \"provider failed\")).retryable()",
        40,
    );
    let added = lines
        .iter()
        .filter(|line| {
            line.spans.first().and_then(|span| span.style.bg) == Some(Color::Rgb(0, 95, 0))
        })
        .map(line_text)
        .collect::<Vec<_>>();

    assert!(added.len() > 1, "expected wrapped added line: {added:?}");
    assert!(added[0].starts_with("     34 +             !StreamFailure"));
    assert!(
        added[1..]
            .iter()
            .all(|line| line.starts_with("                    ")),
        "continuations did not keep gutter and indentation: {added:?}"
    );
    assert_eq!(added.iter().filter(|line| line.contains("34 +")).count(), 1);
}

#[test]
fn diff_header_and_context_are_explicitly_wrapped_to_terminal_width() {
    let width = 18;
    let lines = preview_lines(
        "\npath/to/an/extremely/long/file.rs | -1 | +1\n    9   context text that continues for a while",
        width,
    );

    assert!(
        lines
            .iter()
            .take(lines.len() - 1)
            .all(|line| { UnicodeWidthStr::width(line_text(line).as_str()) <= usize::from(width) })
    );
    let texts = lines.iter().map(line_text).collect::<Vec<_>>();
    assert!(texts.iter().any(|line| line.starts_with("          ")));
}

#[test]
fn default_theme_keeps_diff_text_inheriting_terminal_foreground() {
    let theme = Theme::default();
    assert_eq!(theme.diff_removed, None);
    assert_eq!(theme.diff_added, None);

    let lines = preview_lines("\nfile.rs | -1 | +1\n   12 - old\n   12 + new", 40);
    let removed = lines
        .iter()
        .find(|line| line.spans[0].style.bg == Some(theme.diff_removed_bg))
        .expect("removed diff line");
    let added = lines
        .iter()
        .find(|line| line.spans[0].style.bg == Some(theme.diff_added_bg))
        .expect("added diff line");

    assert_eq!(removed.spans[0].style.fg, None);
    assert_eq!(added.spans[0].style.fg, None);
}

#[test]
fn diff_text_colors_apply_over_the_band_fill_when_set() {
    let mut theme = Theme::default();
    theme.diff_removed = Some(Color::Rgb(155, 17, 30));
    theme.diff_removed_bg = Color::Rgb(192, 192, 192);
    theme.diff_added = Some(Color::Rgb(158, 206, 106));
    theme.diff_added_bg = Color::Rgb(58, 58, 58);

    let lines = preview_lines_with("\nfile.rs | -1 | +1\n   12 - old\n   12 + new", 40, &theme);
    let removed = lines
        .iter()
        .find(|line| line.spans[0].style.bg == Some(theme.diff_removed_bg))
        .expect("removed diff line");
    let added = lines
        .iter()
        .find(|line| line.spans[0].style.bg == Some(theme.diff_added_bg))
        .expect("added diff line");

    assert_eq!(removed.spans[0].style.fg, Some(Color::Rgb(155, 17, 30)));
    assert_eq!(added.spans[0].style.fg, Some(Color::Rgb(158, 206, 106)));
}

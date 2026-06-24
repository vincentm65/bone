use bone::chat::{Message, ToolDisplay};
use bone::ui::render::messages::{msg_to_lines, render_tool};
use bone::ui::theme::Theme;
use ratatui::text::Line;
use unicode_width::UnicodeWidthStr;

fn line_text(line: &Line<'static>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
}

#[test]
fn tool_label_preserves_explicit_newlines_and_indentation() {
    let tool = ToolDisplay {
        label: "shell cd repo &&\n  cargo test".to_string(),
        is_error: false,
    };
    let mut lines = Vec::new();

    render_tool(&tool, "", 0, &Theme::default(), &mut lines, 80);

    let rendered = lines.iter().map(line_text).collect::<Vec<_>>();
    assert_eq!(rendered, vec!["    shell cd repo &&", "      cargo test"]);
}

#[test]
fn tool_label_wraps_multiline_labels_at_narrow_width() {
    let tool = ToolDisplay {
        label: "shell verylongcommand --with-long-argument".to_string(),
        is_error: false,
    };
    let mut lines = Vec::new();

    render_tool(&tool, "", 0, &Theme::default(), &mut lines, 16);

    let rendered = lines.iter().map(line_text).collect::<Vec<_>>();
    assert_eq!(
        rendered,
        vec![
            "    shell",
            "     verylongcom",
            "    mand",
            "     --with-long",
            "    -argument"
        ]
    );
}

#[test]
fn user_multiline_content_does_not_add_prefixes_that_force_rewrap() {
    let second_line = format!("    {}", "a".repeat(81));
    let message = Message::user(format!("heading\n{second_line}"));

    let rendered = msg_to_lines(&[message], &Theme::default(), None, 86)
        .iter()
        .map(line_text)
        .collect::<Vec<_>>();

    assert!(rendered[0].starts_with("> heading"));
    assert_eq!(rendered[1].trim_end(), second_line);
    assert_eq!(
        rendered.len(),
        3,
        "expected content rows plus message spacer"
    );
}

#[test]
fn wrapped_user_indentation_is_preserved_on_continuation_rows() {
    let message = Message::user("heading\n    alpha beta gamma delta");

    let rendered = msg_to_lines(&[message], &Theme::default(), None, 18)
        .iter()
        .map(line_text)
        .collect::<Vec<_>>();

    assert!(rendered[1].starts_with("    alpha"));
    assert!(rendered[2].starts_with("    gamma"));
}

#[test]
fn user_rows_are_not_extended_with_background_padding() {
    let rendered = msg_to_lines(&[Message::user("alpha\nbeta")], &Theme::default(), None, 20)
        .iter()
        .map(line_text)
        .collect::<Vec<_>>();

    assert_eq!(rendered[0], "> alpha");
    assert_eq!(rendered[1], "beta");
}

#[test]
fn user_blank_lines_are_rendered_once_without_terminal_column_wrap() {
    let rendered = msg_to_lines(
        &[Message::user("alpha\n\nbeta")],
        &Theme::default(),
        None,
        20,
    )
    .iter()
    .map(line_text)
    .collect::<Vec<_>>();

    assert_eq!(rendered.len(), 4, "three content rows plus message spacer");
    assert_eq!(rendered[1], "");
}

#[test]
fn deeply_indented_user_rows_still_leave_the_final_column_unpainted() {
    let message = Message::user(format!("heading\n{}content", " ".repeat(30)));
    let rendered = msg_to_lines(&[message], &Theme::default(), None, 20)
        .iter()
        .map(line_text)
        .collect::<Vec<_>>();

    assert!(
        rendered[..rendered.len() - 1]
            .iter()
            .all(|line| UnicodeWidthStr::width(line.as_str()) <= 19),
        "submitted user rows must not trigger terminal auto-wrap: {rendered:?}"
    );
}

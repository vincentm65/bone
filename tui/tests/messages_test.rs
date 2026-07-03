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

fn span_color(line: &Line<'static>, text: &str) -> Option<ratatui::style::Color> {
    line.spans
        .iter()
        .find(|span| span.content.as_ref() == text)
        .and_then(|span| span.style.fg)
}

#[test]
fn file_tool_labels_highlight_path_and_mute_summary() {
    let theme = Theme::default();
    for name in ["read_file", "write_file", "edit_file"] {
        let tool = ToolDisplay {
            label: format!("{name} src/main.rs (lines 1-10, 10 read)"),
            is_error: false,
            is_shell: false,
        };
        let mut lines = Vec::new();
        render_tool(&tool, "", 0, &theme, &mut lines, 80, false);

        let first = &lines[0];
        assert_eq!(
            span_color(first, name),
            Some(ratatui::style::Color::White),
            "{name} name accent"
        );
        assert_eq!(
            span_color(first, " src/main.rs"),
            Some(theme.shell_path),
            "{name} path color"
        );
        assert_eq!(
            span_color(first, " (lines 1-10, 10 read)"),
            Some(theme.tool_call),
            "{name} summary color"
        );
    }
}

#[test]
fn wrapped_file_tool_label_keeps_only_path_colored() {
    let theme = Theme::default();
    let tool = ToolDisplay {
        label: "read_file /home/vincent/projects/bone/core/src/tools/edit_file/diff.rs (lines 1-102, 102 read)".to_string(),
        is_error: false,
        is_shell: false,
    };
    let mut lines = Vec::new();
    render_tool(&tool, "", 0, &theme, &mut lines, 80, false);

    assert_eq!(
        line_text(&lines[0]),
        "    read_file /home/vincent/projects/bone/core/src/tools/edit_file/diff.rs"
    );
    assert_eq!(line_text(&lines[1]), "     (lines 1-102, 102 read)");
    assert_eq!(
        lines[1].spans.last().and_then(|span| span.style.fg),
        Some(theme.tool_call)
    );
}

#[test]
fn non_file_tool_labels_keep_plain_rest_style() {
    let theme = Theme::default();
    let tool = ToolDisplay {
        label: "web_search rust ratatui".to_string(),
        is_error: false,
        is_shell: false,
    };
    let mut lines = Vec::new();
    render_tool(&tool, "", 0, &theme, &mut lines, 80, false);

    assert_eq!(
        span_color(&lines[0], " rust ratatui"),
        Some(theme.tool_call)
    );
}

#[test]
fn tool_label_preserves_explicit_newlines_and_indentation() {
    let tool = ToolDisplay {
        label: "shell cd repo &&\n  cargo test".to_string(),
        is_error: false,
        is_shell: false,
    };
    let mut lines = Vec::new();

    render_tool(&tool, "", 0, &Theme::default(), &mut lines, 80, false);

    let rendered = lines.iter().map(line_text).collect::<Vec<_>>();
    assert_eq!(rendered, vec!["    shell cd repo &&", "      cargo test"]);
}

#[test]
fn tool_label_wraps_multiline_labels_at_narrow_width() {
    let tool = ToolDisplay {
        label: "shell verylongcommand --with-long-argument".to_string(),
        is_error: false,
        is_shell: false,
    };
    let mut lines = Vec::new();

    render_tool(&tool, "", 0, &Theme::default(), &mut lines, 16, false);

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

    let rendered = msg_to_lines(&[message], &Theme::default(), None, 86, false)
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

    let rendered = msg_to_lines(&[message], &Theme::default(), None, 18, false)
        .iter()
        .map(line_text)
        .collect::<Vec<_>>();

    assert!(rendered[1].starts_with("    alpha"));
    assert!(rendered[2].starts_with("    gamma"));
}

#[test]
fn user_rows_are_not_extended_with_background_padding() {
    let rendered = msg_to_lines(
        &[Message::user("alpha\nbeta")],
        &Theme::default(),
        None,
        20,
        false,
    )
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
        false,
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
    let rendered = msg_to_lines(&[message], &Theme::default(), None, 20, false)
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

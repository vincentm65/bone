use super::render_tool;
use crate::chat::ToolDisplay;
use crate::ui::theme::Theme;
use ratatui::text::Line;

fn line_text(line: &Line<'static>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
}

#[test]
fn tool_label_preserves_explicit_newlines_and_indentation() {
    let tool = ToolDisplay {
        label: "bash cd repo &&\n  cargo test".to_string(),
        is_error: false,
    };
    let mut lines = Vec::new();

    render_tool(&tool, "", &Theme::default(), &mut lines, 80);

    let rendered = lines.iter().map(line_text).collect::<Vec<_>>();
    assert_eq!(rendered, vec!["    bash cd repo &&", "      cargo test"]);
}

#[test]
fn tool_label_wraps_multiline_labels_at_narrow_width() {
    let tool = ToolDisplay {
        label: "bash verylongcommand --with-long-argument".to_string(),
        is_error: false,
    };
    let mut lines = Vec::new();

    render_tool(&tool, "", &Theme::default(), &mut lines, 16);

    let rendered = lines.iter().map(line_text).collect::<Vec<_>>();
    assert_eq!(
        rendered,
        vec![
            "    bash",
            "     verylongcom",
            "    mand",
            "     --with-long",
            "    -argument"
        ]
    );
}

use super::*;

const HASHLINE: &str = "Edited: /tmp/a.txt (-1 +1)\n1#10|alpha\n2#4v|BETA\n3#13|gamma";
const PREVIEW: &str = "    edit_file /tmp/a.txt (-1 | +1)\n    2 - beta\n    2 + BETA";

fn edit_result(content: &str, preview: Option<&str>, is_error: bool) -> ChatMessage {
    ChatMessage::tool(ToolResult {
        call_id: "call_1".into(),
        name: "edit_file".into(),
        content: content.into(),
        is_error,
        edit_preview: preview.map(Into::into),
        ..Default::default()
    })
}

#[test]
fn reloaded_edit_shows_saved_preview_not_hashline_content() {
    let rows = transcript_rows(&[edit_result(HASHLINE, Some(PREVIEW), false)], |_| None);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].role, ChatRole::System);
    assert_eq!(rows[0].content, PREVIEW);
    assert!(!rows[0].content.contains("2#4v|"));
}

#[test]
fn failed_edit_ignores_preview() {
    let rows = transcript_rows(&[edit_result("boom", Some(PREVIEW), true)], |_| None);
    assert!(rows.iter().all(|row| row.content != PREVIEW));
}

#[test]
fn legacy_edit_still_shows_embedded_diff() {
    let legacy = format!("Edited: /tmp/a.txt\n{PREVIEW}");
    let rows = transcript_rows(&[edit_result(&legacy, None, false)], |_| None);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].content.contains("2 + BETA"));
}

use bone::tools::types::ToolCall;
use bone::ui::app::stream::{
    assistant_message, call_row_shown_during_prepare, pane_toggle_hint, tool_error,
};
use serde_json::json;

#[test]
fn tool_error_creates_error_result() {
    let call = ToolCall {
        id: "call-1".to_string(),
        name: "shell".to_string(),
        arguments: json!({}),
    };
    let result = tool_error(&call, "something went wrong");
    assert_eq!(result.call_id, "call-1");
    assert_eq!(result.name, "shell");
    assert_eq!(result.content, "something went wrong");
    assert!(result.is_error);
    assert!(result.pane_page.is_none());
}

#[test]
fn pane_toggle_hint_hidden_when_no_pages() {
    assert_eq!(pane_toggle_hint(true, false), None);
    assert_eq!(pane_toggle_hint(false, false), None);
}

#[test]
fn pane_toggle_hint_shows_when_pages_exist() {
    assert_eq!(pane_toggle_hint(true, true), Some("Ctrl+T hide panel"));
    assert_eq!(pane_toggle_hint(false, true), Some("Ctrl+T show panel"));
}

#[test]
fn call_row_shown_during_prepare_only_for_edit_file() {
    let edit_call = ToolCall {
        id: "c1".to_string(),
        name: "edit_file".to_string(),
        arguments: json!({}),
    };
    let shell_call = ToolCall {
        id: "c2".to_string(),
        name: "shell".to_string(),
        arguments: json!({}),
    };
    assert!(call_row_shown_during_prepare(&edit_call));
    assert!(!call_row_shown_during_prepare(&shell_call));
}

#[test]
fn assistant_message_with_tool_calls() {
    let call = ToolCall {
        id: "c1".to_string(),
        name: "shell".to_string(),
        arguments: json!({}),
    };
    let msg = assistant_message(
        "I will run a command".to_string(),
        vec![call],
        "reasoning".to_string(),
    );
    assert_eq!(msg.role, bone::llm::ChatRole::Assistant);
    assert_eq!(msg.content, "I will run a command");
    assert_eq!(msg.reasoning_content, Some("reasoning".to_string()));
    assert_eq!(msg.tool_calls.len(), 1);
}

#[test]
fn assistant_message_without_tool_calls() {
    let msg = assistant_message("Hello".to_string(), vec![], String::new());
    assert_eq!(msg.role, bone::llm::ChatRole::Assistant);
    assert!(msg.tool_calls.is_empty());
    assert!(msg.reasoning_content.is_none());
}

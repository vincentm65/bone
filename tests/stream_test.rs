use bone::tools::types::ToolCall;
use bone::ui::app::stream::{
    assistant_message, call_row_shown_during_prepare, show_immediate_tool_row,
    tool_error,
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
fn immediate_tool_rows_skip_read_file_and_edit_file() {
    let read_call = ToolCall {
        id: "c1".to_string(),
        name: "read_file".to_string(),
        arguments: json!({}),
    };
    let edit_call = ToolCall {
        id: "c2".to_string(),
        name: "edit_file".to_string(),
        arguments: json!({}),
    };
    let shell_call = ToolCall {
        id: "c3".to_string(),
        name: "shell".to_string(),
        arguments: json!({}),
    };

    assert!(!show_immediate_tool_row(&read_call));
    assert!(!show_immediate_tool_row(&edit_call));
    assert!(show_immediate_tool_row(&shell_call));
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
        Some(bone::llm::Reasoning {
            text: "reasoning".to_string(),
            echo_field: Some("reasoning_content".to_string()),
        }),
    );
    assert_eq!(msg.role, bone::llm::ChatRole::Assistant);
    assert_eq!(msg.content, "I will run a command");
    let reasoning = msg.reasoning.expect("reasoning present");
    assert_eq!(reasoning.text, "reasoning");
    assert_eq!(reasoning.echo_field.as_deref(), Some("reasoning_content"));
    assert_eq!(msg.tool_calls.len(), 1);
}

#[test]
fn assistant_message_without_tool_calls() {
    let msg = assistant_message("Hello".to_string(), vec![], None);
    assert_eq!(msg.role, bone::llm::ChatRole::Assistant);
    assert!(msg.tool_calls.is_empty());
    assert!(msg.reasoning.is_none());
}

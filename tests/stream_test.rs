use bone::tools::types::ToolCall;
use bone::ui::app::stream::tool_error;
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

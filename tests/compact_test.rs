use bone::chat::{COMPACT_NOTICE, DEFAULT_KEEP_MESSAGES, find_compact_boundary};
use bone::llm::{ChatMessage, ChatRole};
use bone::tools::ToolCall;

fn tool_call(id: &str) -> ToolCall {
    ToolCall {
        id: id.to_string(),
        name: "read_file".to_string(),
        arguments: serde_json::json!({"path": "Cargo.toml"}),
    }
}

fn tool_result(id: &str, content: &str) -> ChatMessage {
    ChatMessage {
        role: ChatRole::Tool,
        content: content.to_string(),
        tool_calls: Vec::new(),
        tool_call_id: Some(id.to_string()),
        name: Some("read_file".to_string()),
        reasoning_content: None,
    }
}

#[test]
fn find_boundary_returns_none_for_short_transcripts() {
    let messages = vec![ChatMessage::new(ChatRole::User, "hello")];

    let boundary = find_compact_boundary(&messages, DEFAULT_KEEP_MESSAGES);

    assert_eq!(boundary, None);
}

#[test]
fn find_boundary_returns_some_when_compaction_needed() {
    let messages = (0..20)
        .map(|i| ChatMessage::new(ChatRole::User, format!("message {i}")))
        .collect::<Vec<_>>();

    let boundary = find_compact_boundary(&messages, DEFAULT_KEEP_MESSAGES);

    assert_eq!(boundary, Some(8));
}

#[test]
fn find_boundary_respects_custom_keep_count() {
    let messages = (0..100)
        .map(|i| ChatMessage::new(ChatRole::User, format!("msg {i}")))
        .collect::<Vec<_>>();

    let boundary = find_compact_boundary(&messages, 5);

    assert_eq!(boundary, Some(95));
}

#[test]
fn find_boundary_returns_none_when_at_limit() {
    let messages = (0..12)
        .map(|i| ChatMessage::new(ChatRole::User, format!("msg {i}")))
        .collect::<Vec<_>>();

    let boundary = find_compact_boundary(&messages, DEFAULT_KEEP_MESSAGES);

    assert_eq!(boundary, None);
}

#[test]
fn find_boundary_does_not_orphan_tool_result() {
    let messages = vec![
        ChatMessage::new(ChatRole::User, "please read"),
        ChatMessage::assistant_with_tools("", vec![tool_call("call-1")]),
        tool_result("call-1", "tool result"),
        ChatMessage::new(ChatRole::Assistant, "done"),
    ];

    // keep=2, requested boundary=2 -> boundary adjusted back past tool_result
    let boundary = find_compact_boundary(&messages, 2);

    assert_eq!(boundary, Some(1));
}

#[test]
fn find_boundary_keeps_complete_multi_tool_chain() {
    let messages = vec![
        ChatMessage::new(ChatRole::User, "please read two files"),
        ChatMessage::assistant_with_tools("", vec![tool_call("call-1"), tool_call("call-2")]),
        tool_result("call-1", "first result"),
        tool_result("call-2", "second result"),
        ChatMessage::new(ChatRole::Assistant, "done"),
    ];

    let boundary = find_compact_boundary(&messages, 2);

    assert_eq!(boundary, Some(1));
}

#[test]
fn find_boundary_does_not_split_chain_when_boundary_after_tool_result() {
    let messages = vec![
        ChatMessage::new(ChatRole::User, "before"),
        ChatMessage::new(ChatRole::User, "please read"),
        ChatMessage::assistant_with_tools("", vec![tool_call("call-1")]),
        tool_result("call-1", "tool result"),
        ChatMessage::new(ChatRole::Assistant, "done"),
    ];

    let boundary = find_compact_boundary(&messages, 2);

    assert_eq!(boundary, Some(2));
}

#[test]
fn compact_notice_is_constant() {
    assert_eq!(COMPACT_NOTICE, "Compacted older messages.");
}

use bone::chat::{COMPACT_NOTICE, DEFAULT_KEEP_MESSAGES, compact_transcript};
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
fn compact_keeps_short_transcripts_unchanged() {
    let messages = vec![ChatMessage::new(ChatRole::User, "hello")];

    let compacted = compact_transcript(&messages, DEFAULT_KEEP_MESSAGES);

    assert_eq!(compacted.len(), 1);
    assert!(matches!(compacted, std::borrow::Cow::Borrowed(_)));
    assert_eq!(compacted[0].content, "hello");
}

#[test]
fn compact_replaces_old_messages_with_notice() {
    let messages = (0..20)
        .map(|i| ChatMessage::new(ChatRole::User, format!("message {i}")))
        .collect::<Vec<_>>();

    let compacted = compact_transcript(&messages, DEFAULT_KEEP_MESSAGES);

    assert_eq!(compacted.len(), 13);
    assert!(matches!(compacted, std::borrow::Cow::Owned(_)));
    assert_eq!(compacted[0].content, COMPACT_NOTICE);
    assert_eq!(compacted[0].role, ChatRole::System);
    assert_eq!(compacted[1].content, "message 8");
    assert_eq!(compacted[12].content, "message 19");
}

#[test]
fn compact_custom_keep_count() {
    let messages = (0..100)
        .map(|i| ChatMessage::new(ChatRole::User, format!("msg {i}")))
        .collect::<Vec<_>>();

    // Keep only 5 messages
    let compacted = compact_transcript(&messages, 5);
    assert_eq!(compacted.len(), 6); // 1 notice + 5 kept
    assert_eq!(compacted[1].content, "msg 95");
    assert_eq!(compacted[5].content, "msg 99");
}

#[test]
fn compact_no_op_when_at_limit() {
    let messages = (0..12)
        .map(|i| ChatMessage::new(ChatRole::User, format!("msg {i}")))
        .collect::<Vec<_>>();

    let compacted = compact_transcript(&messages, DEFAULT_KEEP_MESSAGES);

    assert_eq!(compacted.len(), 12);
    assert!(matches!(compacted, std::borrow::Cow::Borrowed(_)));
}

#[test]
fn compact_preserves_message_order() {
    let messages = (0..50)
        .map(|i| ChatMessage::new(ChatRole::User, format!("msg {i}")))
        .collect::<Vec<_>>();

    let compacted = compact_transcript(&messages, 10);
    // Kept messages should be the last 10, in original order
    for (i, expected) in (40..50).zip(compacted.iter().skip(1)) {
        assert_eq!(expected.content, format!("msg {i}"));
    }
}

#[test]
fn compact_zero_keep_preserves_at_least_one_message() {
    let messages = (0..3)
        .map(|i| ChatMessage::new(ChatRole::User, format!("msg {i}")))
        .collect::<Vec<_>>();

    let compacted = compact_transcript(&messages, 0);

    assert_eq!(compacted.len(), 2);
    assert_eq!(compacted[1].content, "msg 2");
}

#[test]
fn compact_does_not_start_with_orphan_tool_result() {
    let messages = vec![
        ChatMessage::new(ChatRole::User, "please read"),
        ChatMessage::assistant_with_tools("", vec![tool_call("call-1")]),
        tool_result("call-1", "tool result"),
        ChatMessage::new(ChatRole::Assistant, "done"),
    ];

    let compacted = compact_transcript(&messages, 2);

    assert_eq!(compacted[1].role, ChatRole::Assistant);
    assert!(!compacted[1].tool_calls.is_empty());
    assert_eq!(compacted[2].role, ChatRole::Tool);
}

#[test]
fn compact_keeps_complete_multi_tool_chain() {
    let messages = vec![
        ChatMessage::new(ChatRole::User, "please read two files"),
        ChatMessage::assistant_with_tools("", vec![tool_call("call-1"), tool_call("call-2")]),
        tool_result("call-1", "first result"),
        tool_result("call-2", "second result"),
        ChatMessage::new(ChatRole::Assistant, "done"),
    ];

    let compacted = compact_transcript(&messages, 2);

    assert_eq!(compacted[1].role, ChatRole::Assistant);
    assert_eq!(compacted[1].tool_calls.len(), 2);
    assert_eq!(compacted[2].tool_call_id.as_deref(), Some("call-1"));
    assert_eq!(compacted[3].tool_call_id.as_deref(), Some("call-2"));
    assert_eq!(compacted[4].content, "done");
}

#[test]
fn compact_does_not_split_chain_when_boundary_after_tool_result() {
    let messages = vec![
        ChatMessage::new(ChatRole::User, "before"),
        ChatMessage::new(ChatRole::User, "please read"),
        ChatMessage::assistant_with_tools("", vec![tool_call("call-1")]),
        tool_result("call-1", "tool result"),
        ChatMessage::new(ChatRole::Assistant, "done"),
    ];

    let compacted = compact_transcript(&messages, 2);

    assert_eq!(compacted[1].role, ChatRole::Assistant);
    assert_eq!(compacted[2].tool_call_id.as_deref(), Some("call-1"));
    assert_eq!(compacted[3].content, "done");
}

#[test]
fn compact_notice_is_constant() {
    assert_eq!(COMPACT_NOTICE, "Compacted older messages.");
}

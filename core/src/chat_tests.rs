use super::*;
use crate::tools::ToolCall;

#[test]
fn provider_history_adds_tool_timing_without_mutating_transcript() {
    let mut assistant = ChatMessage::new(ChatRole::Assistant, "checking");
    assistant.created_at = Some("2026-07-17T12:00:00Z".into());
    assistant.tool_calls.push(ToolCall {
        id: "call-1".into(),
        name: "shell".into(),
        arguments: serde_json::json!({"command": "sleep 5"}),
    });
    let mut tool = ChatMessage::new(ChatRole::Tool, "done");
    tool.tool_call_id = Some("call-1".into());
    tool.name = Some("shell".into());
    tool.created_at = Some("2026-07-17T12:00:05Z".into());
    let transcript = vec![assistant, tool];

    let history = build_chat_history(&transcript, "system");

    assert_eq!(transcript[0].content, "checking");
    assert_eq!(transcript[1].content, "done");
    assert_eq!(history[1].content, "checking");
    assert_eq!(history[1].output_sequence, transcript[0].output_sequence);
    assert!(history[2].content.contains(
        "Tool timing: requested at 2026-07-17T12:00:00Z; completed at 2026-07-17T12:00:05Z."
    ));
}

#[test]
fn provider_timing_normalization_is_idempotent() {
    let mut user = ChatMessage::new(ChatRole::User, "hello");
    user.created_at = Some("2026-07-17T12:00:00Z".into());

    let once = provider_facing_messages(&[user]);
    let twice = provider_facing_messages(&once);

    assert_eq!(twice, once);
    assert_eq!(twice[0].content.matches("<timing>").count(), 1);
}

#[test]
fn provider_history_does_not_add_timing_to_assistant_output() {
    let mut assistant = ChatMessage::new(ChatRole::Assistant, "");
    assistant.created_at = Some("2026-07-17T12:00:00Z".into());
    assistant.output_sequence = vec![crate::llm::OutputItem::ToolCall(ToolCall {
        id: "call-1".into(),
        name: "shell".into(),
        arguments: serde_json::json!({"command": "true"}),
    })];

    let history = build_chat_history(&[assistant.clone()], "system");

    assert_eq!(history[1].content, "");
    assert_eq!(history[1].output_sequence, assistant.output_sequence);
    let codex_items = crate::llm::providers::codex::build_codex_messages(history);
    assert!(
        !serde_json::to_string(&codex_items)
            .unwrap()
            .contains("<timing>")
    );
}

// ── Tool-call sequence repair ───────────────────────────────────────────────

fn call(id: &str, name: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: serde_json::json!({}),
    }
}

fn tool_reply(id: &str, name: &str, content: &str) -> ChatMessage {
    let mut message = ChatMessage::new(ChatRole::Tool, content);
    message.tool_call_id = Some(id.into());
    message.name = Some(name.into());
    message
}

fn image_relay(name: &str) -> ChatMessage {
    let mut message = ChatMessage::new(ChatRole::User, format!("Image output from {name}:"));
    message.synthetic = true;
    message
}

fn roles(messages: &[ChatMessage]) -> Vec<ChatRole> {
    messages.iter().map(|m| m.role).collect()
}

fn ids(messages: &[ChatMessage]) -> Vec<Option<&str>> {
    messages.iter().map(|m| m.tool_call_id.as_deref()).collect()
}

#[test]
fn repair_moves_image_relays_after_the_tool_batch() {
    let mut messages = vec![
        ChatMessage::assistant_with_tools(
            "",
            vec![call("call-a", "read_file"), call("call-b", "read_file")],
        ),
        tool_reply("call-a", "read_file", "a"),
        image_relay("read_file"),
        tool_reply("call-b", "read_file", "b"),
        image_relay("read_file"),
    ];

    repair_tool_call_sequences(&mut messages);

    assert_eq!(
        roles(&messages),
        vec![
            ChatRole::Assistant,
            ChatRole::Tool,
            ChatRole::Tool,
            ChatRole::User,
            ChatRole::User,
        ]
    );
    // Replies stay in call order, ids preserved.
    assert_eq!(
        ids(&messages),
        vec![None, Some("call-a"), Some("call-b"), None, None]
    );
    // No data invented or dropped: both relays survive, after the batch.
    assert_eq!(messages[3].content, "Image output from read_file:");
    assert_eq!(messages[4].content, "Image output from read_file:");
    assert_eq!(messages[1].content, "a");
    assert_eq!(messages[2].content, "b");
}

#[test]
fn repair_treats_legacy_prefix_relays_as_relays() {
    // A row written before the `synthetic` flag existed: recognized by prefix.
    let mut messages = vec![
        ChatMessage::assistant_with_tools("", vec![call("call-a", "read_file")]),
        ChatMessage::new(ChatRole::User, "Image output from read_file:"),
        tool_reply("call-a", "read_file", "a"),
    ];

    repair_tool_call_sequences(&mut messages);

    assert_eq!(
        ids(&messages),
        vec![None, Some("call-a"), None],
        "relay must move behind its tool reply"
    );
    assert_eq!(messages[1].content, "a");
}

#[test]
fn repair_synthesizes_missing_tool_replies_in_call_order() {
    let mut messages = vec![
        ChatMessage::assistant_with_tools(
            "",
            vec![call("call-a", "read_file"), call("call-b", "read_file")],
        ),
        // Only b replied; a's turn was interrupted.
        tool_reply("call-b", "read_file", "b only"),
    ];

    repair_tool_call_sequences(&mut messages);

    assert_eq!(messages.len(), 3);
    assert_eq!(messages[1].tool_call_id.as_deref(), Some("call-a"));
    assert!(messages[1].is_error);
    assert_eq!(messages[1].name.as_deref(), Some("read_file"));
    assert_eq!(
        messages[1].content,
        "Tool call was interrupted; no result was recorded."
    );
    assert_eq!(messages[2].tool_call_id.as_deref(), Some("call-b"));
    assert!(!messages[2].is_error);
    assert_eq!(messages[2].content, "b only");
}

#[test]
fn repair_leaves_valid_sequences_unchanged() {
    let mut messages = vec![
        ChatMessage::assistant_with_tools("", vec![call("call-a", "shell")]),
        tool_reply("call-a", "shell", "ok"),
        ChatMessage::new(ChatRole::User, "thanks"),
    ];
    let before = messages.clone();

    repair_tool_call_sequences(&mut messages);

    assert_eq!(messages, before);
}

#[test]
fn repair_is_idempotent() {
    let mut messages = vec![
        ChatMessage::assistant_with_tools(
            "",
            vec![call("call-a", "read_file"), call("call-b", "read_file")],
        ),
        tool_reply("call-a", "read_file", "a"),
        image_relay("read_file"),
        tool_reply("call-b", "read_file", "b"),
        image_relay("read_file"),
    ];

    repair_tool_call_sequences(&mut messages);
    let once = messages.clone();
    repair_tool_call_sequences(&mut messages);

    assert_eq!(messages, once);
}

#[test]
fn repair_ignores_replies_belonging_to_other_calls() {
    // An orphan tool reply (no matching assistant call) must be left alone, and
    // must not be absorbed into a later assistant's batch.
    let mut messages = vec![
        tool_reply("orphan", "shell", "stray"),
        ChatMessage::assistant_with_tools("", vec![call("call-a", "shell")]),
        tool_reply("call-a", "shell", "ok"),
    ];
    let before = messages.clone();

    repair_tool_call_sequences(&mut messages);

    assert_eq!(messages, before);
}

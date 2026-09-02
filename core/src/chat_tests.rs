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

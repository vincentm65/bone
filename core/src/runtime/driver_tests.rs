use super::*;

#[test]
fn empty_turn_messages_leave_request_history_unchanged() {
    let mut request_history = vec![ChatMessage::new(ChatRole::User, "hello")];
    append_turn_messages(&mut request_history, &[]);

    assert_eq!(request_history.len(), 1);
    assert_eq!(request_history[0].role, ChatRole::User);
    assert_eq!(request_history[0].content, "hello");
}

#[test]
fn turn_messages_append_to_last_tool_result_mid_loop() {
    let mut request_history = vec![
        ChatMessage::new(ChatRole::User, "do it"),
        ChatMessage::new(ChatRole::Tool, "exit code: 0\nstdout:\nalpha"),
    ];
    append_turn_messages(&mut request_history, &["remember".to_string()]);

    assert_eq!(request_history.len(), 2);
    assert_eq!(request_history[1].role, ChatRole::Tool);
    assert_eq!(
        request_history[1].content,
        "exit code: 0\nstdout:\nalpha\n\n<system-reminder>\nremember\n</system-reminder>"
    );
}

#[test]
fn turn_messages_after_user_append_trailing_user_message() {
    let mut request_history = vec![ChatMessage::new(ChatRole::User, "do it")];
    append_turn_messages(&mut request_history, &["remember".to_string()]);

    assert_eq!(request_history.len(), 2);
    assert_eq!(request_history[0].role, ChatRole::User);
    assert_eq!(request_history[0].content, "do it");
    assert_eq!(request_history[1].role, ChatRole::User);
    assert_eq!(
        request_history[1].content,
        "<system-reminder>\nremember\n</system-reminder>"
    );
}

#[test]
fn history_rebuild_restores_ephemeral_image_relay() {
    let image = crate::llm::ImageData {
        media_type: "image/png".to_string(),
        data: "png-data".to_string(),
        width: Some(1920),
        height: Some(1080),
        sha256: Some("abc123def456".into()),
    };
    let relay = ChatMessage::user_with_images("screenshot", vec![image]);
    let mut request_history = vec![ChatMessage::new(ChatRole::User, "rebuilt")];
    let mut ephemeral_relay = Some((99, relay));

    restore_ephemeral_image_relay(&mut request_history, &mut ephemeral_relay);

    assert_eq!(ephemeral_relay.as_ref().map(|(index, _)| *index), Some(1));
    assert_eq!(request_history.len(), 2);
    assert_eq!(request_history[1].images[0].data, "png-data");
    assert_eq!(request_history[1].images[0].width, Some(1920));
    assert_eq!(request_history[1].images[0].height, Some(1080));
    assert_eq!(
        request_history[1].images[0].sha256.as_deref(),
        Some("abc123def456")
    );
}

fn message_with_call(role: ChatRole, content: &str, id: &str) -> ChatMessage {
    let mut message = ChatMessage::new(role, content);
    message.tool_calls.push(ToolCall {
        id: id.to_string(),
        name: "read_file".to_string(),
        arguments: serde_json::json!({}),
    });
    message
}

fn tool_result(content: &str, id: Option<&str>) -> ChatMessage {
    let mut message = ChatMessage::new(ChatRole::Tool, content);
    message.tool_call_id = id.map(str::to_string);
    message
}

#[test]
fn compaction_suffix_retains_current_and_requested_recent_turns() {
    let messages = vec![
        ChatMessage::new(ChatRole::User, "u1"),
        ChatMessage::new(ChatRole::Assistant, "a1"),
        ChatMessage::new(ChatRole::User, "u2"),
        ChatMessage::new(ChatRole::Assistant, "a2"),
        ChatMessage::new(ChatRole::User, "u3"),
        ChatMessage::new(ChatRole::Assistant, "a3"),
        ChatMessage::new(ChatRole::User, "current"),
    ];

    assert_eq!(compaction_suffix_start(&messages, 2), 2);
    assert_eq!(compaction_suffix_start(&messages, 0), 6);
    assert_eq!(compaction_suffix_start(&messages, usize::MAX), 0);
}

#[test]
fn compaction_suffix_widens_to_keep_linked_tool_chain() {
    let messages = vec![
        ChatMessage::new(ChatRole::User, "u1"),
        message_with_call(ChatRole::Assistant, "calling", "call-1"),
        ChatMessage::new(ChatRole::User, "current"),
        tool_result("late result", Some("call-1")),
    ];

    assert_eq!(compaction_suffix_start(&messages, 0), 0);
}

#[test]
fn compaction_suffix_keeps_all_history_for_ambiguous_tool_results() {
    let missing_id = vec![
        ChatMessage::new(ChatRole::User, "old"),
        ChatMessage::new(ChatRole::Assistant, "done"),
        ChatMessage::new(ChatRole::User, "current"),
        tool_result("result", None),
    ];
    assert_eq!(compaction_suffix_start(&missing_id, 0), 0);

    let duplicate_calls = vec![
        ChatMessage::new(ChatRole::User, "old"),
        message_with_call(ChatRole::Assistant, "first", "same"),
        ChatMessage::new(ChatRole::User, "current"),
        message_with_call(ChatRole::Assistant, "second", "same"),
        tool_result("result", Some("same")),
    ];
    assert_eq!(compaction_suffix_start(&duplicate_calls, 0), 0);
}

#[test]
fn compaction_prompt_and_checkpoint_are_stable_plain_user_content() {
    let initial = compaction_prompt("preserve decisions", 4, false);
    assert!(initial.contains("final 4 transcript messages"));
    assert!(initial.contains("preserve decisions"));
    assert!(initial.contains("unfinished work, and next actions"));
    assert!(initial.contains("immediately continue the current user request"));
    assert!(!initial.contains("previous checkpoint was invalid"));

    let repair = compaction_prompt("preserve decisions", 4, true);
    assert!(repair.contains("only repair attempt"));
    assert!(repair.contains("do not call tools"));

    let checkpoint = checkpoint_message("  concise state  ");
    assert_eq!(checkpoint.role, ChatRole::User);
    assert_eq!(
        checkpoint.content,
        format!("{COMPACTION_CHECKPOINT_PREFIX}concise state{COMPACTION_CONTINUATION}")
    );
    assert!(checkpoint.content.contains("active task is not complete"));
    assert!(
        checkpoint
            .content
            .contains("Continue with the next needed actions")
    );
}

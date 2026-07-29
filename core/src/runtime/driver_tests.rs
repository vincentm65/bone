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
    };
    let relay = ChatMessage::user_with_images("screenshot", vec![image]);
    let mut request_history = vec![ChatMessage::new(ChatRole::User, "rebuilt")];
    let mut ephemeral_relay = Some((99, relay));

    restore_ephemeral_image_relay(&mut request_history, &mut ephemeral_relay);

    assert_eq!(ephemeral_relay.as_ref().map(|(index, _)| *index), Some(1));
    assert_eq!(request_history.len(), 2);
    assert_eq!(request_history[1].images[0].data, "png-data");
}

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
fn history_rebuild_restores_ephemeral_image_relays_in_order() {
    let image = |data: &str| crate::llm::ImageData {
        media_type: "image/png".to_string(),
        data: data.to_string(),
        width: Some(1920),
        height: Some(1080),
        sha256: Some("abc123def456".into()),
    };
    let relays = vec![
        ChatMessage::user_with_images("first", vec![image("png-data-1")]),
        ChatMessage::user_with_images("second", vec![image("png-data-2")]),
    ];
    let mut request_history = vec![ChatMessage::new(ChatRole::User, "rebuilt")];

    restore_ephemeral_image_relays(&mut request_history, &relays);

    assert_eq!(request_history.len(), 3);
    assert_eq!(request_history[1].images[0].data, "png-data-1");
    assert_eq!(request_history[2].images[0].data, "png-data-2");
    assert_eq!(request_history[1].images[0].width, Some(1920));
    assert_eq!(request_history[1].images[0].height, Some(1080));
    assert_eq!(
        request_history[1].images[0].sha256.as_deref(),
        Some("abc123def456")
    );
}

use super::*;

#[test]
fn context_system_prompt_prefers_provider_history() {
    let history = vec![ChatMessage::new(
        ChatRole::System,
        "base prompt\n\nhook append",
    )];

    assert_eq!(
        context_system_prompt(&history, &Some("base prompt".into())).as_deref(),
        Some("base prompt\n\nhook append")
    );
}

#[test]
fn context_system_prompt_falls_back_when_history_has_no_system_message() {
    let history = vec![ChatMessage::new(ChatRole::User, "hello")];

    assert_eq!(
        context_system_prompt(&history, &Some("base prompt".into())).as_deref(),
        Some("base prompt")
    );
}

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
fn pending_context_estimate_includes_ephemeral_image_relays() {
    use crate::llm::ImageData;

    let history = vec![ChatMessage::new(ChatRole::User, "hello")];
    let relay_without_image = ChatMessage::user_with_images("image relay", vec![]);
    let relay_with_image = ChatMessage::user_with_images(
        "image relay",
        vec![ImageData {
            media_type: "image/png".to_string(),
            width: Some(448),
            height: Some(448),
            ..Default::default()
        }],
    );
    let mut request_history = history.clone();
    request_history.push(relay_with_image);

    let durable_estimate = super::estimate_context_chars(&history, 0);
    let pending_estimate = super::estimate_context_chars(&request_history, 0);
    let pending_without_image =
        super::estimate_context_chars(&[history[0].clone(), relay_without_image], 0);

    assert!(pending_estimate > durable_estimate);
    assert_eq!(pending_estimate - pending_without_image, 973);
}

#[test]
fn ephemeral_image_relay_replacement_preserves_durable_relays() {
    fn image_relay(label: &str) -> ChatMessage {
        let mut message = ChatMessage::user_with_images(
            format!("Image output from {label}:"),
            vec![crate::llm::ImageData {
                media_type: "image/png".into(),
                data: label.into(),
                ..Default::default()
            }],
        );
        message.synthetic = true;
        message
    }

    let durable = image_relay("durable");
    let old = image_relay("old");
    let newest = image_relay("newest");
    let mut request_history = vec![durable.clone(), old.clone()];
    let mut relays = vec![old];
    let mut relay_index = Some(1);

    replace_ephemeral_image_relay(
        &mut request_history,
        &mut relays,
        &mut relay_index,
        newest.clone(),
    );

    assert_eq!(request_history, vec![durable.clone(), newest.clone()]);
    assert_eq!(relays, vec![newest.clone()]);
    assert_eq!(relay_index, Some(1));

    let mut rebuilt = vec![durable.clone()];
    let restored_index =
        restore_ephemeral_image_relays(&mut rebuilt, &[image_relay("old"), newest.clone()]);
    assert_eq!(rebuilt, vec![durable, newest]);
    assert_eq!(restored_index, Some(1));
}

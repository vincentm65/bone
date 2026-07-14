use super::*;

#[test]
fn turn_messages_empty_clones_history() {
    let history = vec![ChatMessage::new(ChatRole::User, "hello")];
    let shaped = apply_turn_messages(&history, &[]);

    assert_eq!(shaped.len(), 1);
    assert_eq!(shaped[0].role, ChatRole::User);
    assert_eq!(shaped[0].content, "hello");
}

#[test]
fn turn_messages_append_to_last_tool_result_mid_loop() {
    let history = vec![
        ChatMessage::new(ChatRole::User, "do it"),
        ChatMessage::new(ChatRole::Tool, "exit code: 0\nstdout:\nalpha"),
    ];
    let shaped = apply_turn_messages(&history, &["remember".to_string()]);

    assert_eq!(shaped.len(), 2);
    assert_eq!(shaped[1].role, ChatRole::Tool);
    assert_eq!(
        shaped[1].content,
        "exit code: 0\nstdout:\nalpha\n\n<system-reminder>\nremember\n</system-reminder>"
    );
    assert_eq!(history[1].content, "exit code: 0\nstdout:\nalpha");
}

#[test]
fn turn_messages_after_user_append_trailing_user_message() {
    let history = vec![ChatMessage::new(ChatRole::User, "do it")];
    let shaped = apply_turn_messages(&history, &["remember".to_string()]);

    assert_eq!(shaped.len(), 2);
    assert_eq!(shaped[0].role, ChatRole::User);
    assert_eq!(shaped[0].content, "do it");
    assert_eq!(shaped[1].role, ChatRole::User);
    assert_eq!(
        shaped[1].content,
        "<system-reminder>\nremember\n</system-reminder>"
    );
}

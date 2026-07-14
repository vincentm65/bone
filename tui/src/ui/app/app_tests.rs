use super::should_open_agent_log;
use crate::ui::input::InputState;

#[test]
fn agent_log_enter_opens_log_with_empty_input() {
    assert!(should_open_agent_log(&InputState::default()));
}

#[test]
fn agent_log_enter_submits_nonempty_input() {
    let mut input = InputState::default();
    input.buffer = "queue this message".into();

    assert!(!should_open_agent_log(&input));
}

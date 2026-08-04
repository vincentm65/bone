use super::*;

#[test]
fn decision_prioritizes_blocks_then_mode() {
    for (blocked, allowed, expected) in [
        (None, true, CallOutcome::Approve),
        (None, false, CallOutcome::Denied),
        (
            Some("blocked".to_string()),
            true,
            CallOutcome::Blocked("blocked".to_string()),
        ),
        (
            Some("blocked".to_string()),
            false,
            CallOutcome::Blocked("blocked".to_string()),
        ),
    ] {
        assert_eq!(decide_call(blocked, allowed), expected);
    }
}

#[test]
fn denied_message_includes_mode_and_safety() {
    assert_eq!(
        denied_message(ApprovalMode::Safe, CommandSafety::Danger),
        "Tool skipped. Approval mode safe does not allow Danger; continue using allowed read-only tools or report the limitation."
    );
}

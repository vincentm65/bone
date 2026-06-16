//! Unit tests for the extracted approval decision (`decide_call`).
//!
//! These verify the policy in isolation — no `ToolHandler`, no
//! `ExtensionManager`, no Lua, no SQLite — confirming the extraction is a
//! faithful, injectable seam. They also pin the exact "Tool skipped" message
//! so the refactor preserves behavior byte-for-byte.

use bone::tools::ApprovalMode;
use bone::tools::approval::{CallOutcome, decide_call, denied_message};
use bone::tools::command_policy::CommandSafety;

#[test]
fn approve_when_allowed_and_not_blocked() {
    assert_eq!(decide_call(None, true), CallOutcome::Approve);
}

#[test]
fn deny_when_not_allowed_and_not_blocked() {
    assert_eq!(decide_call(None, false), CallOutcome::Denied);
}

#[test]
fn block_takes_precedence_over_allow() {
    // A hook veto wins even if the mode would allow the call.
    assert_eq!(
        decide_call(Some("user vetoed".into()), true),
        CallOutcome::Blocked("user vetoed".into())
    );
}

#[test]
fn block_takes_precedence_over_deny() {
    // Hook veto wins over mode-deny too.
    assert_eq!(
        decide_call(Some("blocked".into()), false),
        CallOutcome::Blocked("blocked".into())
    );
}

#[test]
fn denied_message_matches_original_format_exactly() {
    // Byte-for-byte match with the inline format! that lived in agent.rs.
    let msg = denied_message(ApprovalMode::Safe, CommandSafety::Danger);
    assert_eq!(
        msg,
        "[exit_code=1] Tool skipped. Approval mode safe does not allow Danger; \
         continue using allowed read-only tools or report the limitation."
    );

    let msg2 = denied_message(ApprovalMode::Safe, CommandSafety::ReadOnly);
    assert!(msg2.contains("does not allow ReadOnly"));
}

#[test]
fn denied_message_uses_mode_str() {
    let danger_mode = denied_message(ApprovalMode::Danger, CommandSafety::ReadOnly);
    assert!(danger_mode.starts_with("[exit_code=1] Tool skipped. Approval mode danger "));
}

use serde_json::json;

use super::ApprovalMode;
use crate::tools::types::ToolCall;

fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
    ToolCall {
        id: "id".to_string(),
        name: name.to_string(),
        arguments,
    }
}

#[test]
fn safe_mode_only_allows_read_only() {
    assert!(ApprovalMode::Safe.allows_call(&call("read_file", json!({ "path": "Cargo.toml" }))));
    assert!(ApprovalMode::Safe.allows_call(&call(
        "bash",
        json!({ "command": "pwd", "classification": "read_only" })
    )));
    assert!(!ApprovalMode::Safe.allows_call(&call("edit_file", json!({ "path": "Cargo.toml" }))));
    assert!(!ApprovalMode::Safe.allows_call(&call(
        "bash",
        json!({ "command": "cargo fmt", "classification": "edit" })
    )));
}

#[test]
fn edit_mode_allows_read_only_and_edit() {
    assert!(ApprovalMode::Edits.allows_call(&call(
        "bash",
        json!({ "command": "cargo fmt", "classification": "edit" })
    )));
    assert!(!ApprovalMode::Edits.allows_call(&call(
        "bash",
        json!({ "command": "rm -rf target", "classification": "danger" })
    )));
}

#[test]
fn danger_mode_only_blocks_dangerous_git_bash_commands() {
    assert!(ApprovalMode::Danger.allows_call(&call(
        "bash",
        json!({ "command": "rm -rf target", "classification": "danger" })
    )));
    assert!(ApprovalMode::Danger.allows_call(&call(
        "bash",
        json!({ "command": "git status", "classification": "read_only" })
    )));
    assert!(ApprovalMode::Danger.allows_call(&call(
        "bash",
        json!({ "command": "git diff", "classification": "read_only" })
    )));
    assert!(!ApprovalMode::Danger.allows_call(&call(
        "bash",
        json!({ "command": "cd repo && git commit -am x", "classification": "danger" })
    )));
    assert!(!ApprovalMode::Danger.allows_call(&call(
        "bash",
        json!({ "command": "git push", "classification": "danger" })
    )));
}

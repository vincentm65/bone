use serde_json::json;

use bone::tools::ApprovalMode;
use bone::tools::ToolCall;

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
        "shell",
        json!({ "command": "pwd", "classification": "read_only" })
    )));
    assert!(!ApprovalMode::Safe.allows_call(&call("edit_file", json!({ "path": "Cargo.toml" }))));
    assert!(!ApprovalMode::Safe.allows_call(&call(
        "shell",
        json!({ "command": "cargo fmt", "classification": "edit" })
    )));
}

#[test]
fn danger_mode_allows_all() {
    assert!(ApprovalMode::Danger.allows_call(&call(
        "shell",
        json!({ "command": "rm -rf target", "classification": "danger" })
    )));
    assert!(ApprovalMode::Danger.allows_call(&call(
        "shell",
        json!({ "command": "git status", "classification": "read_only" })
    )));
    assert!(ApprovalMode::Danger.allows_call(&call(
        "shell",
        json!({ "command": "git diff", "classification": "read_only" })
    )));
    assert!(ApprovalMode::Danger.allows_call(&call(
        "shell",
        json!({ "command": "cd repo && git commit -am x", "classification": "danger" })
    )));
    assert!(ApprovalMode::Danger.allows_call(&call(
        "shell",
        json!({ "command": "git push", "classification": "danger" })
    )));
}

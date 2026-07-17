use serde_json::json;

use bone_core::tools::command_policy::CommandSafety;
use bone_core::tools::{ApprovalMode, ToolCall};

fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
    ToolCall {
        id: "id".to_string(),
        name: name.to_string(),
        arguments,
    }
}

fn allows(mode: ApprovalMode, name: &str, arguments: serde_json::Value) -> bool {
    mode.allows_safety(CommandSafety::for_call(&call(name, arguments)))
}

#[test]
fn safe_mode_only_allows_read_only() {
    assert!(allows(
        ApprovalMode::Safe,
        "read_file",
        json!({ "path": "Cargo.toml" })
    ));
    assert!(allows(
        ApprovalMode::Safe,
        "shell",
        json!({ "command": "pwd", "classification": "read_only" })
    ));
    assert!(!allows(
        ApprovalMode::Safe,
        "edit_file",
        json!({ "path": "Cargo.toml" })
    ));
    assert!(!allows(
        ApprovalMode::Safe,
        "shell",
        json!({ "command": "cargo fmt", "classification": "edit" })
    ));
}

#[test]
fn danger_mode_allows_all() {
    for command in [
        "rm -rf target",
        "git status",
        "git diff",
        "cd repo && git commit -am x",
        "git push",
    ] {
        assert!(allows(
            ApprovalMode::Danger,
            "shell",
            json!({ "command": command })
        ));
    }
}

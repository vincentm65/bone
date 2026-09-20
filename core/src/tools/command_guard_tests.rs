//! Pure string-classification tests for the destructive-command guard.
//!
//! These tests never spawn a process, never execute a command, and never call
//! [`GuardRoots::detect`] — the roots are fixed fakes, so nothing here touches
//! the real home directory or the config directory.

use super::*;
use std::path::PathBuf;

fn roots() -> GuardRoots {
    GuardRoots {
        home: Some(PathBuf::from("/home/example")),
        workspace: Some(PathBuf::from("/home/example/projects/bone")),
    }
}

fn assert_denied(command: &str) {
    assert!(
        hard_deny(command, &roots()).is_some(),
        "expected guard to deny: {command}"
    );
}

fn assert_allowed(command: &str) {
    assert!(
        hard_deny(command, &roots()).is_none(),
        "expected guard to allow: {command}"
    );
}

#[test]
fn denies_home_and_root_deletions() {
    for command in [
        "rm -rf ~",
        "rm -rf $HOME",
        "rm -rf ${HOME}",
        "rm -rf /",
        "rm -rf /*",
        "rm -rf /home/example",
        "rm -rf ~/",
        "rm -rf ..",
        "rm -rf ../..",
        "rm -rf .",
        "rm -rf *",
        "rm -rf target/*",
        "rm -rf",
    ] {
        assert_denied(command);
    }
}

#[test]
fn denies_shell_wrappers_and_privilege_escalation() {
    for command in [
        "bash -c 'rm -rf ~'",
        "sh -c \"rm -rf $HOME\"",
        "sudo rm -rf /home/example",
        "env rm -rf ~",
        "cd ~ && rm -rf .",
    ] {
        assert_denied(command);
    }
}

#[test]
fn denies_wrappers_after_connectors_and_nested_wrappers() {
    for command in [
        "true && bash -c 'dd if=/dev/zero of=/dev/sda'",
        "sudo bash -c 'rm -rf /home/example'",
    ] {
        assert_denied(command);
    }
}

#[test]
fn denies_command_substitutions_and_backticks() {
    for command in ["echo $(rm -rf /)", "echo `rm -rf /`"] {
        assert_denied(command);
    }
}

#[test]
fn denies_shell_escapes_that_reconstruct_dangerous_tokens() {
    for command in [
        r"r\m -rf /home/example",
        r"r''m -rf /home/example",
        r"rm -rf ./\../bone",
    ] {
        assert_denied(command);
    }
}

#[test]
fn command_path_flag_does_not_consume_the_command() {
    assert_denied("command -p dd if=/dev/zero of=/dev/sda");
}

#[test]
fn denies_disk_system_and_nested_operations() {
    for command in [
        "mv ~ /tmp/x",
        "find ~ -delete",
        "find / -delete",
        "dd if=/dev/zero of=~/disk.img",
        "dd if=/dev/zero of=/dev/sda",
        "mkfs.ext4 /dev/sda1",
        "truncate -s 0 ~/.bashrc",
        "echo x > ~/.bashrc",
        "> /etc/passwd",
        "find ~ -print0 | xargs -0 rm -rf",
        "git clean -fdx",
        "chmod -R 777 ~",
        ":(){ :|:& };:",
    ] {
        assert_denied(command);
    }
}

#[test]
fn allows_workspace_scoped_commands() {
    for command in [
        "rm -rf target",
        "rm -rf target/",
        "rm -f src/main.rs",
        "rm -rf /tmp/bone-guard-canary",
        "ls -la ~",
        "cat ~/.bashrc",
        "grep -rn \"rm\" /etc/hosts",
        "git commit -m \"fix rm -rf bug\"",
        "cargo build",
        "touch notes.md",
        "mv target/a target/b",
        "chmod +x script.sh",
        "cmd 2>/dev/null",
        "find target -name '*.tmp' -delete",
    ] {
        assert_allowed(command);
    }
}

#[test]
fn denies_guard_self_test_canary() {
    for command in [
        "bone-guard-selftest",
        "sudo bone-guard-selftest",
        "sh -c 'bone-guard-selftest'",
        "true && bone-guard-selftest",
        "bone-guard-selftest --now",
    ] {
        assert_denied(command);
    }
}

#[test]
fn canary_matches_whole_token_only() {
    // A word that merely contains the canary name is a different token, so it is
    // not refused: the canary check is exact, not a substring match.
    assert_allowed("echo bone-guard-selftest-backup");
}

#[test]
fn hard_deny_call_only_inspects_shell_commands() {
    let roots = roots();
    let shell_call = |arguments: serde_json::Value| ToolCall {
        id: "call".into(),
        name: "shell".into(),
        arguments,
    };

    assert!(hard_deny_call(&shell_call(serde_json::json!({ "command": "rm -rf ~" })), &roots).is_some());
    assert!(hard_deny_call(&shell_call(serde_json::json!({ "action": "run", "command": "rm -rf ~" })), &roots).is_some());
    assert!(hard_deny_call(&shell_call(serde_json::json!({ "action": "kill", "id": "1" })), &roots).is_none());
    assert!(hard_deny_call(&shell_call(serde_json::json!({ "action": "list" })), &roots).is_none());
    assert!(hard_deny_call(&shell_call(serde_json::json!({ "command": "rm -rf target" })), &roots).is_none());

    let edit_call = ToolCall {
        id: "call".into(),
        name: "edit_file".into(),
        arguments: serde_json::json!({ "path": "~/notes.md" }),
    };
    assert!(hard_deny_call(&edit_call, &roots).is_none());
}

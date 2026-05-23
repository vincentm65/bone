use serde_json::json;

use super::{CommandSafety, minimum_required_classification};
use crate::tools::approval::ApprovalMode;
use crate::tools::types::ToolCall;

fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
    ToolCall {
        id: "id".to_string(),
        name: name.to_string(),
        arguments,
    }
}

#[test]
fn missing_bash_classification_is_danger() {
    assert_eq!(
        CommandSafety::from_tool_call(&call("bash", json!({ "command": "pwd" }))),
        CommandSafety::Danger
    );
}

// ------------------------------------------------------------------
// minimum_required_classification policy tests
// ------------------------------------------------------------------

#[test]
fn policy_danger_rm() {
    assert_eq!(
        minimum_required_classification("rm -rf /"),
        CommandSafety::Danger
    );
    assert_eq!(
        minimum_required_classification("rm something"),
        CommandSafety::Danger
    );
}

#[test]
fn policy_danger_sudo() {
    assert_eq!(
        minimum_required_classification("sudo apt install foo"),
        CommandSafety::Danger
    );
    assert_eq!(
        minimum_required_classification("sudo rm -rf /"),
        CommandSafety::Danger
    );
}

#[test]
fn policy_danger_chmod_chown() {
    assert_eq!(
        minimum_required_classification("chmod 777 foo"),
        CommandSafety::Danger
    );
    assert_eq!(
        minimum_required_classification("chown user:group bar"),
        CommandSafety::Danger
    );
}

#[test]
fn policy_danger_systemctl() {
    assert_eq!(
        minimum_required_classification("systemctl stop nginx"),
        CommandSafety::Danger
    );
    assert_eq!(
        minimum_required_classification("systemctl restart nginx"),
        CommandSafety::Danger
    );
    assert_eq!(
        minimum_required_classification("systemctl disable nginx"),
        CommandSafety::Danger
    );
    // Non-destructive systemctl is at least Edit
    assert_eq!(
        minimum_required_classification("systemctl status nginx"),
        CommandSafety::Edit
    );
}

#[test]
fn policy_danger_service() {
    assert_eq!(
        minimum_required_classification("service nginx stop"),
        CommandSafety::Danger
    );
    assert_eq!(
        minimum_required_classification("service nginx restart"),
        CommandSafety::Danger
    );
    // Non-destructive service is at least Edit
    assert_eq!(
        minimum_required_classification("service nginx status"),
        CommandSafety::Edit
    );
}

#[test]
fn policy_danger_curl_wget_output() {
    assert_eq!(
        minimum_required_classification("curl -O http://example.com/file"),
        CommandSafety::Danger
    );
    assert_eq!(
        minimum_required_classification("wget -O file http://example.com"),
        CommandSafety::Danger
    );
    assert_eq!(
        minimum_required_classification("curl -o /tmp/f http://example.com"),
        CommandSafety::Danger
    );
    // Plain curl/wget is at least Edit (network access)
    assert_eq!(
        minimum_required_classification("curl http://example.com"),
        CommandSafety::Edit
    );
    assert_eq!(
        minimum_required_classification("wget http://example.com"),
        CommandSafety::Edit
    );
}

#[test]
fn policy_danger_redirection_to_system_paths() {
    assert_eq!(
        minimum_required_classification("echo foo > /etc/bar"),
        CommandSafety::Danger
    );
    assert_eq!(
        minimum_required_classification("cat file >> /etc/config"),
        CommandSafety::Danger
    );
    // /dev/ redirections are harmless but may still be edit based on command.
    assert_eq!(
        minimum_required_classification("echo foo > /dev/null"),
        CommandSafety::ReadOnly
    );
    assert_eq!(
        minimum_required_classification("rg approval 2>/dev/null"),
        CommandSafety::ReadOnly
    );
}

#[test]
fn policy_danger_git_destructive() {
    assert_eq!(
        minimum_required_classification("git push origin main"),
        CommandSafety::Danger
    );
    assert_eq!(
        minimum_required_classification("git reset --hard HEAD~1"),
        CommandSafety::Danger
    );
    assert_eq!(
        minimum_required_classification("git checkout main"),
        CommandSafety::Danger
    );
    assert_eq!(
        minimum_required_classification("git rebase main"),
        CommandSafety::Danger
    );
    assert_eq!(
        minimum_required_classification("git clean -fd"),
        CommandSafety::Danger
    );
    assert_eq!(
        minimum_required_classification("git commit -am x"),
        CommandSafety::Danger
    );
    assert_eq!(
        minimum_required_classification("git switch main"),
        CommandSafety::Danger
    );
    assert_eq!(
        minimum_required_classification("git restore file"),
        CommandSafety::Danger
    );
    assert_eq!(
        minimum_required_classification("git merge feature"),
        CommandSafety::Danger
    );
    assert_eq!(
        minimum_required_classification("git pull"),
        CommandSafety::Danger
    );
    assert_eq!(
        minimum_required_classification("git tag v1"),
        CommandSafety::Danger
    );
}

#[test]
fn policy_edit_mv_cp_mkdir_touch_tee() {
    assert_eq!(
        minimum_required_classification("mv a b"),
        CommandSafety::Edit
    );
    assert_eq!(
        minimum_required_classification("cp a b"),
        CommandSafety::Edit
    );
    assert_eq!(
        minimum_required_classification("mkdir foo"),
        CommandSafety::Edit
    );
    assert_eq!(
        minimum_required_classification("touch foo"),
        CommandSafety::Edit
    );
    assert_eq!(
        minimum_required_classification("tee file.txt"),
        CommandSafety::Edit
    );
}

#[test]
fn policy_edit_package_managers() {
    assert_eq!(
        minimum_required_classification("apt install curl"),
        CommandSafety::Edit
    );
    assert_eq!(
        minimum_required_classification("pacman -Syu"),
        CommandSafety::Edit
    );
    assert_eq!(
        minimum_required_classification("brew install rust"),
        CommandSafety::Edit
    );
    assert_eq!(
        minimum_required_classification("pip install requests"),
        CommandSafety::Edit
    );
    assert_eq!(
        minimum_required_classification("npm install express"),
        CommandSafety::Edit
    );
    assert_eq!(
        minimum_required_classification("cargo install ripgrep"),
        CommandSafety::Edit
    );
}

#[test]
fn policy_edit_redirections() {
    assert_eq!(
        minimum_required_classification("echo hello > file.txt"),
        CommandSafety::Edit
    );
    assert_eq!(
        minimum_required_classification("cat a >> b"),
        CommandSafety::Edit
    );
    assert_eq!(
        minimum_required_classification("some_cmd | tee log.txt"),
        CommandSafety::Edit
    );
}

#[test]
fn policy_edit_sed_inplace() {
    assert_eq!(
        minimum_required_classification("sed -i 's/foo/bar/' file.txt"),
        CommandSafety::Edit
    );
    assert_eq!(
        minimum_required_classification("sed -i.bak 's/foo/bar/' file.txt"),
        CommandSafety::Edit
    );
    assert_eq!(
        minimum_required_classification("sed --in-place 's/foo/bar/' file.txt"),
        CommandSafety::Edit
    );
}

#[test]
fn policy_readonly_allowlist() {
    for cmd in &[
        "ls -la",
        "pwd",
        "cat file.txt",
        "head -5 file.txt",
        "tail -5 file.txt",
        "rg pattern .",
        "grep pattern file",
        "find . -name '*.rs'",
        "wc -l file",
        "sort file",
        "uniq file",
        "echo hello",
        "which rustc",
        "env",
        "printenv",
        "date",
        "whoami",
        "id",
        "uname -a",
        "du -sh target",
        "df -h",
        "ps aux",
        "file Cargo.toml",
        "stat Cargo.toml",
        "realpath Cargo.toml",
        "basename src/main.rs",
        "dirname src/main.rs",
        "tree -L 2",
    ] {
        assert_eq!(
            minimum_required_classification(cmd),
            CommandSafety::ReadOnly,
            "expected ReadOnly for: {cmd}"
        );
    }
}

#[test]
fn policy_readonly_cargo() {
    assert_eq!(
        minimum_required_classification("cargo check"),
        CommandSafety::ReadOnly
    );
    assert_eq!(
        minimum_required_classification("cargo test"),
        CommandSafety::ReadOnly
    );
    assert_eq!(
        minimum_required_classification("cargo build"),
        CommandSafety::ReadOnly
    );
    assert_eq!(
        minimum_required_classification("cargo --version"),
        CommandSafety::ReadOnly
    );
}

#[test]
fn policy_readonly_git() {
    assert_eq!(
        minimum_required_classification("git status"),
        CommandSafety::ReadOnly
    );
    assert_eq!(
        minimum_required_classification("git log"),
        CommandSafety::ReadOnly
    );
    assert_eq!(
        minimum_required_classification("git diff"),
        CommandSafety::ReadOnly
    );
    assert_eq!(
        minimum_required_classification("git branch"),
        CommandSafety::ReadOnly
    );
    assert_eq!(
        minimum_required_classification("git show HEAD"),
        CommandSafety::ReadOnly
    );
    assert_eq!(
        minimum_required_classification("git ls-files"),
        CommandSafety::ReadOnly
    );
}

/// Model misclassifications are caught: even if the model says read_only,
/// the effective classification must be at least the policy minimum.
#[test]
fn policy_overrides_model_misclassification() {
    // Model says read_only but rm is always Danger
    let c1 = call(
        "bash",
        json!({ "command": "rm -rf /", "classification": "read_only" }),
    );
    assert!(!ApprovalMode::Safe.allows_call(&c1));
    assert!(!ApprovalMode::Edits.allows_call(&c1));
    // Danger mode allows all non-git, but rm is not git → allowed
    assert!(ApprovalMode::Danger.allows_call(&c1));

    // Model says read_only but sudo is always Danger
    let c2 = call(
        "bash",
        json!({ "command": "sudo apt install foo", "classification": "read_only" }),
    );
    assert!(!ApprovalMode::Safe.allows_call(&c2));
    assert!(!ApprovalMode::Edits.allows_call(&c2));
    assert!(ApprovalMode::Danger.allows_call(&c2));

    // Model says read_only but mv is at least Edit
    let c3 = call(
        "bash",
        json!({ "command": "mv a b", "classification": "read_only" }),
    );
    assert!(!ApprovalMode::Safe.allows_call(&c3));
    assert!(ApprovalMode::Edits.allows_call(&c3));
    assert!(ApprovalMode::Danger.allows_call(&c3));

    // Model says read_only but echo > file is at least Edit
    let c4 = call(
        "bash",
        json!({ "command": "echo hello > file.txt", "classification": "read_only" }),
    );
    assert!(!ApprovalMode::Safe.allows_call(&c4));
    assert!(ApprovalMode::Edits.allows_call(&c4));
    assert!(ApprovalMode::Danger.allows_call(&c4));

    // Model says danger but ls is ReadOnly — deterministic policy wins.
    let c5 = call(
        "bash",
        json!({ "command": "ls -la", "classification": "danger" }),
    );
    assert!(ApprovalMode::Safe.allows_call(&c5));
    assert!(ApprovalMode::Edits.allows_call(&c5));
    assert!(ApprovalMode::Danger.allows_call(&c5));
}

/// Bash approval ignores model over-classification and uses deterministic policy.
#[test]
fn bash_policy_is_source_of_truth() {
    let rg_stderr_dev_null = call(
        "bash",
        json!({ "command": "rg -t py --no-filename -l approval 2>/dev/null", "classification": "danger" }),
    );
    assert!(ApprovalMode::Safe.allows_call(&rg_stderr_dev_null));

    let bash_wrapped_rg = call(
        "bash",
        json!({ "command": "bash rg -t py --no-filename -l approval 2>/dev/null", "classification": "read_only" }),
    );
    assert!(!ApprovalMode::Safe.allows_call(&bash_wrapped_rg));

    let git_status = call(
        "bash",
        json!({ "command": "git status", "classification": "danger" }),
    );
    assert!(ApprovalMode::Safe.allows_call(&git_status));

    let rm = call(
        "bash",
        json!({ "command": "rm foo", "classification": "read_only" }),
    );
    assert!(!ApprovalMode::Safe.allows_call(&rm));
    assert!(!ApprovalMode::Edits.allows_call(&rm));

    let ls = call(
        "bash",
        json!({ "command": "ls", "classification": "danger" }),
    );
    assert!(ApprovalMode::Safe.allows_call(&ls));
}

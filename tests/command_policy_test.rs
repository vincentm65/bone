use serde_json::json;

use bone::tools::ApprovalMode;
use bone::tools::ToolCall;
use bone::tools::command_policy::{CommandSafety, classify_command};

fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
    ToolCall {
        id: "id".to_string(),
        name: name.to_string(),
        arguments,
    }
}

#[test]
fn missing_shell_command_is_danger() {
    assert_eq!(
        CommandSafety::for_call(&call("shell", json!({}))),
        CommandSafety::Danger
    );
}

#[test]
fn for_call_ignores_model_classification() {
    // Model says danger but pwd is ReadOnly — policy wins.
    assert_eq!(
        CommandSafety::for_call(&call(
            "shell",
            json!({ "command": "pwd", "classification": "danger" })
        )),
        CommandSafety::ReadOnly
    );
    // Model says read_only but rm is Danger — policy wins.
    assert_eq!(
        CommandSafety::for_call(&call(
            "shell",
            json!({ "command": "rm -rf /", "classification": "read_only" })
        )),
        CommandSafety::Danger
    );
}

// ------------------------------------------------------------------
// classify_command policy tests
// ------------------------------------------------------------------

#[test]
fn policy_danger_rm() {
    assert_eq!(classify_command("rm -rf /"), CommandSafety::Danger);
    assert_eq!(classify_command("rm something"), CommandSafety::Danger);
}

#[test]
fn policy_danger_sudo() {
    assert_eq!(
        classify_command("sudo apt install foo"),
        CommandSafety::Danger
    );
    assert_eq!(classify_command("sudo rm -rf /"), CommandSafety::Danger);
}

#[test]
fn policy_danger_chmod_chown() {
    assert_eq!(classify_command("chmod 777 foo"), CommandSafety::Danger);
    assert_eq!(
        classify_command("chown user:group bar"),
        CommandSafety::Danger
    );
}

#[test]
fn policy_danger_systemctl() {
    assert_eq!(
        classify_command("systemctl stop nginx"),
        CommandSafety::Danger
    );
    assert_eq!(
        classify_command("systemctl restart nginx"),
        CommandSafety::Danger
    );
    assert_eq!(
        classify_command("systemctl disable nginx"),
        CommandSafety::Danger
    );
    // Non-destructive systemctl is at least Edit
    assert_eq!(
        classify_command("systemctl status nginx"),
        CommandSafety::Danger
    );
}

#[test]
fn policy_danger_service() {
    assert_eq!(
        classify_command("service nginx stop"),
        CommandSafety::Danger
    );
    assert_eq!(
        classify_command("service nginx restart"),
        CommandSafety::Danger
    );
    // Non-destructive service is at least Edit
    assert_eq!(
        classify_command("service nginx status"),
        CommandSafety::Danger
    );
}

#[test]
fn policy_danger_curl_wget_output() {
    assert_eq!(
        classify_command("curl -O http://example.com/file"),
        CommandSafety::Danger
    );
    assert_eq!(
        classify_command("wget -O file http://example.com"),
        CommandSafety::Danger
    );
    assert_eq!(
        classify_command("curl -o /tmp/f http://example.com"),
        CommandSafety::Danger
    );
    // Plain curl/wget is at least Edit (network access)
    assert_eq!(
        classify_command("curl http://example.com"),
        CommandSafety::Danger
    );
    assert_eq!(
        classify_command("wget http://example.com"),
        CommandSafety::Danger
    );
}

#[test]
fn policy_danger_redirection_to_system_paths() {
    assert_eq!(
        classify_command("echo foo > /etc/bar"),
        CommandSafety::Danger
    );
    assert_eq!(
        classify_command("cat file >> /etc/config"),
        CommandSafety::Danger
    );
    // /dev/ redirections are harmless but may still be edit based on command.
    assert_eq!(
        classify_command("echo foo > /dev/null"),
        CommandSafety::ReadOnly
    );
    assert_eq!(
        classify_command("rg approval 2>/dev/null"),
        CommandSafety::ReadOnly
    );
}

#[test]
fn policy_danger_git_destructive() {
    assert_eq!(
        classify_command("git push origin main"),
        CommandSafety::Danger
    );
    assert_eq!(
        classify_command("git reset --hard HEAD~1"),
        CommandSafety::Danger
    );
    assert_eq!(classify_command("git checkout main"), CommandSafety::Danger);
    assert_eq!(classify_command("git rebase main"), CommandSafety::Danger);
    assert_eq!(classify_command("git clean -fd"), CommandSafety::Danger);
    assert_eq!(classify_command("git commit -am x"), CommandSafety::Danger);
    assert_eq!(classify_command("git switch main"), CommandSafety::Danger);
    assert_eq!(classify_command("git restore file"), CommandSafety::Danger);
    assert_eq!(classify_command("git merge feature"), CommandSafety::Danger);
    assert_eq!(classify_command("git pull"), CommandSafety::Danger);
    assert_eq!(classify_command("git tag v1"), CommandSafety::Danger);
}

#[test]
fn policy_edit_mv_cp_mkdir_touch_tee() {
    assert_eq!(classify_command("mv a b"), CommandSafety::Danger);
    assert_eq!(classify_command("cp a b"), CommandSafety::Danger);
    assert_eq!(classify_command("mkdir foo"), CommandSafety::Danger);
    assert_eq!(classify_command("touch foo"), CommandSafety::Danger);
    assert_eq!(classify_command("tee file.txt"), CommandSafety::Danger);
}

#[test]
fn policy_edit_package_managers() {
    assert_eq!(classify_command("apt install curl"), CommandSafety::Danger);
    assert_eq!(classify_command("pacman -Syu"), CommandSafety::Danger);
    assert_eq!(classify_command("brew install rust"), CommandSafety::Danger);
    assert_eq!(
        classify_command("pip install requests"),
        CommandSafety::Danger
    );
    assert_eq!(
        classify_command("npm install express"),
        CommandSafety::Danger
    );
    assert_eq!(
        classify_command("cargo install ripgrep"),
        CommandSafety::Danger
    );
}

#[test]
fn policy_edit_redirections() {
    assert_eq!(
        classify_command("echo hello > file.txt"),
        CommandSafety::Danger
    );
    assert_eq!(classify_command("cat a >> b"), CommandSafety::Danger);
    assert_eq!(
        classify_command("some_cmd | tee log.txt"),
        CommandSafety::Danger
    );
}

#[test]
fn policy_edit_sed_inplace() {
    assert_eq!(
        classify_command("sed -i 's/foo/bar/' file.txt"),
        CommandSafety::Danger
    );
    assert_eq!(
        classify_command("sed -i.bak 's/foo/bar/' file.txt"),
        CommandSafety::Danger
    );
    assert_eq!(
        classify_command("sed --in-place 's/foo/bar/' file.txt"),
        CommandSafety::Danger
    );
}

#[test]
fn policy_powershell_mutations_prompt() {
    for cmd in &[
        "Remove-Item file.txt",
        "del file.txt",
        "Copy-Item a b",
        "Move-Item a b",
        "Rename-Item a b",
        "New-Item file.txt",
        "Set-Content file.txt value",
        "Add-Content file.txt value",
        "Out-File file.txt",
        "Tee-Object file.txt",
        "Set-ItemProperty . Name value",
        "Stop-Process -Id 123",
        "Stop-Service Spooler",
    ] {
        assert_ne!(
            classify_command(cmd),
            CommandSafety::ReadOnly,
            "expected prompt-worthy classification for: {cmd}"
        );
    }
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
        "xargs -0 wc -l 2>/dev/null",
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
        "awk '{ print $1 }' file",
        "sed -n '1,10p' file",
        "cut -d: -f1 /etc/passwd",
        "printf hello",
        "sha256sum Cargo.toml",
    ] {
        assert_eq!(
            classify_command(cmd),
            CommandSafety::ReadOnly,
            "expected ReadOnly for: {cmd}"
        );
    }
}

#[test]
fn policy_readonly_powershell_allowlist() {
    for cmd in &[
        "Get-ChildItem src",
        "gci src",
        "dir src",
        "Get-Content Cargo.toml",
        "gc Cargo.toml",
        "Select-String -Path src/*.rs -Pattern ToolCall",
        "sls ToolCall src/*.rs",
        "Measure-Object -Line",
        "Sort-Object Name",
        "Where-Object { $_.Name -like '*.rs' }",
        "ForEach-Object { $_.Name }",
        "Write-Output hello",
        "Get-Location",
        "Set-Location src",
        "Get-Item Cargo.toml",
        "Get-ItemProperty .",
        "Get-Command cargo",
        "Get-Process",
        "Get-Service",
        "Resolve-Path Cargo.toml",
        "Test-Path Cargo.toml",
        "Select-Object Name",
        "Format-Table Name",
        "Out-String",
    ] {
        assert_eq!(
            classify_command(cmd),
            CommandSafety::ReadOnly,
            "expected ReadOnly for: {cmd}"
        );
    }
}

#[test]
fn policy_readonly_cargo() {
    assert_eq!(classify_command("cargo check"), CommandSafety::ReadOnly);
    assert_eq!(classify_command("cargo test"), CommandSafety::ReadOnly);
    assert_eq!(classify_command("cargo build"), CommandSafety::ReadOnly);
    assert_eq!(classify_command("cargo --version"), CommandSafety::ReadOnly);
}

#[test]
fn policy_readonly_git() {
    assert_eq!(classify_command("git status"), CommandSafety::ReadOnly);
    assert_eq!(classify_command("git log"), CommandSafety::ReadOnly);
    assert_eq!(classify_command("git diff"), CommandSafety::ReadOnly);
    assert_eq!(classify_command("git branch"), CommandSafety::ReadOnly);
    assert_eq!(classify_command("git show HEAD"), CommandSafety::ReadOnly);
    assert_eq!(classify_command("git ls-files"), CommandSafety::ReadOnly);
}

/// Shell approval ignores model classification entirely — deterministic policy wins.
#[test]
fn policy_overrides_model_classification() {
    // Model says read_only but rm is always Danger
    let c1 = call(
        "shell",
        json!({ "command": "rm -rf /", "classification": "read_only" }),
    );
    assert!(!ApprovalMode::Safe.allows_call(&c1));
    assert!(ApprovalMode::Danger.allows_call(&c1));

    // Model says read_only but sudo is always Danger
    let c2 = call(
        "shell",
        json!({ "command": "sudo apt install foo", "classification": "read_only" }),
    );
    assert!(!ApprovalMode::Safe.allows_call(&c2));
    assert!(ApprovalMode::Danger.allows_call(&c2));

    // Model says read_only but mv is at least Edit
    let c3 = call(
        "shell",
        json!({ "command": "mv a b", "classification": "read_only" }),
    );
    assert!(!ApprovalMode::Safe.allows_call(&c3));
    assert!(ApprovalMode::Danger.allows_call(&c3));

    // Model says read_only but echo > file is at least Edit
    let c4 = call(
        "shell",
        json!({ "command": "echo hello > file.txt", "classification": "read_only" }),
    );
    assert!(!ApprovalMode::Safe.allows_call(&c4));
    assert!(ApprovalMode::Danger.allows_call(&c4));

    // Model says danger but ls is ReadOnly — deterministic policy wins.
    let c5 = call(
        "shell",
        json!({ "command": "ls -la", "classification": "danger" }),
    );
    assert!(ApprovalMode::Safe.allows_call(&c5));
    assert!(ApprovalMode::Danger.allows_call(&c5));

    // Model says danger but git status is ReadOnly — policy wins.
    let c6 = call(
        "shell",
        json!({ "command": "git status", "classification": "danger" }),
    );
    assert!(ApprovalMode::Safe.allows_call(&c6));

    // Shell classification cannot downgrade dangerous commands.
    // git push is Danger regardless of model claim.
    let c7 = call(
        "shell",
        json!({ "command": "git push", "classification": "read_only" }),
    );
    assert!(!ApprovalMode::Safe.allows_call(&c7));
    assert!(ApprovalMode::Danger.allows_call(&c7));
}

/// Shell approval ignores model over-classification and uses deterministic policy.
#[test]
fn shell_policy_is_source_of_truth() {
    let rg_stderr_dev_null = call(
        "shell",
        json!({ "command": "rg -t py --no-filename -l approval 2>/dev/null", "classification": "danger" }),
    );
    assert!(ApprovalMode::Safe.allows_call(&rg_stderr_dev_null));

    // Shell-wrapper peeling: `bash rg …` should classify as ReadOnly (inner command is rg).
    let shell_wrapped_rg = call(
        "shell",
        json!({ "command": "bash rg -t py --no-filename -l approval 2>/dev/null", "classification": "read_only" }),
    );
    assert!(ApprovalMode::Safe.allows_call(&shell_wrapped_rg));

    let git_status = call(
        "shell",
        json!({ "command": "git status", "classification": "danger" }),
    );
    assert!(ApprovalMode::Safe.allows_call(&git_status));

    let rm = call(
        "shell",
        json!({ "command": "rm foo", "classification": "read_only" }),
    );
    assert!(!ApprovalMode::Safe.allows_call(&rm));
    assert!(ApprovalMode::Danger.allows_call(&rm));

    let ls = call(
        "shell",
        json!({ "command": "ls", "classification": "danger" }),
    );
    assert!(ApprovalMode::Safe.allows_call(&ls));
}

// ------------------------------------------------------------------
// Shell-wrapper peeling tests
// ------------------------------------------------------------------

#[test]
fn peel_bash_prefix() {
    // `bash rg …` → classifies inner `rg` as ReadOnly
    assert_eq!(
        classify_command("bash rg -n struct ToolCall --type rust"),
        CommandSafety::ReadOnly
    );
    assert_eq!(
        classify_command("bash cd /home && rg -n foo"),
        CommandSafety::ReadOnly // `cd` is now in readonly allowlist
    );
}

#[test]
fn peel_sh_c_quoted() {
    assert_eq!(
        classify_command("sh -c \"rg -n foo\""),
        CommandSafety::ReadOnly
    );
    assert_eq!(
        classify_command("sh -c 'rg -n foo'"),
        CommandSafety::ReadOnly
    );
    assert_eq!(
        classify_command("sh -c \"rm -rf /\""),
        CommandSafety::Danger
    );
}

#[test]
fn peel_bash_c_unquoted() {
    assert_eq!(
        classify_command("bash -c rg -n foo"),
        CommandSafety::ReadOnly
    );
}

#[test]
fn peel_preserves_danger_classification() {
    // `bash rm …` must still be Danger
    assert_eq!(classify_command("bash rm -rf /"), CommandSafety::Danger);
    assert_eq!(
        classify_command("bash sudo apt install foo"),
        CommandSafety::Danger
    );
    assert_eq!(
        classify_command("sh -c \"git push\""),
        CommandSafety::Danger
    );
}

#[test]
fn peel_preserves_edit_classification() {
    assert_eq!(classify_command("bash mv a b"), CommandSafety::Danger);
    assert_eq!(classify_command("bash mkdir foo"), CommandSafety::Danger);
    assert_eq!(
        classify_command("zsh -c \"echo hello > file.txt\""),
        CommandSafety::Danger
    );
}

#[test]
fn no_peel_for_direct_commands() {
    // Commands not starting with a shell name are unaffected
    assert_eq!(classify_command("rg -n foo"), CommandSafety::ReadOnly);
    assert_eq!(classify_command("rm foo"), CommandSafety::Danger);
    assert_eq!(classify_command("mv a b"), CommandSafety::Danger);
}

// ------------------------------------------------------------------
// Compound command splitting tests
// ------------------------------------------------------------------

#[test]
fn compound_and_danger() {
    assert_eq!(
        classify_command("cd /tmp && rm -rf /"),
        CommandSafety::Danger
    );
    assert_eq!(
        classify_command("ls -la && rm -rf /"),
        CommandSafety::Danger
    );
}

#[test]
fn compound_and_edit() {
    assert_eq!(
        classify_command("echo hello && mv a b"),
        CommandSafety::Danger
    );
    assert_eq!(
        classify_command("cd /tmp && mkdir foo"),
        CommandSafety::Danger
    );
}

#[test]
fn compound_and_readonly() {
    assert_eq!(classify_command("ls -la && pwd"), CommandSafety::ReadOnly);
    assert_eq!(
        classify_command("cd /tmp && ls -la && rg foo"),
        CommandSafety::ReadOnly
    );
}

#[test]
fn compound_or_and_semicolon() {
    assert_eq!(
        classify_command("ls -la || rm -rf /"),
        CommandSafety::Danger
    );
    assert_eq!(classify_command("ls -la ; rm -rf /"), CommandSafety::Danger);
    assert_eq!(classify_command("ls -la ; pwd"), CommandSafety::ReadOnly);
}

#[test]
fn compound_newlines_comments_and_pipes_readonly() {
    let command = r#"
      # Lines in dedicated test files (tests/ directory + *_test.rs in src/)
    echo "=== Dedicated test files ==="
    find /home/vincent/projects/bone -type f \( -path "*/tests/*" -o -name "*_test.rs" \) -not -path "*/target/*" -not -path "*/.git/*" -print0 |
      xargs -0 wc -l 2>/dev/null |
      sort -n

      # Lines inside #[cfg(test)] modules within src/ (non-test files)
    echo "=== Test lines inside src/ (non-test files) ==="
    rg -n '#\[cfg\(test\)\]' /home/vincent/projects/bone/src/ --no-filename
"#;

    assert_eq!(classify_command(command), CommandSafety::ReadOnly);
}

#[test]
fn compound_readonly_while_loop_pipeline() {
    let command = r#"
    find /home/vincent/projects/bone/src -name "*.rs" |
      sort |
      while read f;
      do echo "$(wc -l < "$f") $f";
      done |
      sort -rn
"#;

    assert_eq!(classify_command(command), CommandSafety::ReadOnly);
}

#[test]
fn compound_splitting_ignores_quoted_pipes_and_hashes() {
    assert_eq!(
        classify_command("echo \"a | b # c\"\nrg '#\\[cfg\\(test\\)\\]' src"),
        CommandSafety::ReadOnly
    );
}

#[test]
fn policy_powershell_wrappers_peel_to_inner_command() {
    assert_eq!(
        classify_command("pwsh -NoProfile -Command \"Get-ChildItem src | Sort-Object Name\""),
        CommandSafety::ReadOnly
    );
    assert_eq!(
        classify_command("powershell.exe -NonInteractive -Command \"Remove-Item file.txt\""),
        CommandSafety::Danger
    );
}

#[test]
fn policy_powershell_pipeline_readonly() {
    let command = r#"
    Get-ChildItem src -Recurse -Filter *.rs |
      Where-Object { $_.Length -gt 0 } |
      ForEach-Object { $_.FullName } |
      Sort-Object
"#;

    assert_eq!(classify_command(command), CommandSafety::ReadOnly);
}

#[test]
fn compound_with_shell_wrapper() {
    assert_eq!(
        classify_command("bash ls -la && rm -rf /"),
        CommandSafety::Danger
    );
    assert_eq!(
        classify_command("sh -c 'ls -la ; rm -rf /'"),
        CommandSafety::Danger
    );
    assert_eq!(
        classify_command("bash cd /tmp && ls -la"),
        CommandSafety::ReadOnly
    );
}

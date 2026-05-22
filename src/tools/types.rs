use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Safety classification supplied with shell commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandSafety {
    /// Read-only inspection commands that do not modify files, services, network state, or git state.
    ReadOnly,
    /// Commands that create, update, or delete project files, install dependencies, or otherwise mutate normal workspace state.
    Edit,
    /// Destructive, privileged, external side-effecting, or otherwise high-risk commands.
    Danger,
}

impl CommandSafety {
    /// Numeric rank for comparing severity: ReadOnly=0, Edit=1, Danger=2.
    fn rank(self) -> u8 {
        match self {
            Self::ReadOnly => 0,
            Self::Edit => 1,
            Self::Danger => 2,
        }
    }

    /// Return the more restrictive (higher-ranked) classification.
    pub fn max(self, other: Self) -> Self {
        if self.rank() >= other.rank() {
            self
        } else {
            other
        }
    }

    pub fn from_tool_call(call: &ToolCall) -> Self {
        match call.name.as_str() {
            "read_file" => Self::ReadOnly,
            "write_file" | "edit_file" => Self::Edit,
            "bash" => call
                .arguments
                .get("classification")
                .and_then(Value::as_str)
                .and_then(|value| match value {
                    "read_only" => Some(Self::ReadOnly),
                    "edit" => Some(Self::Edit),
                    "danger" => Some(Self::Danger),
                    _ => None,
                })
                // Missing or malformed classifications are treated as dangerous.
                .unwrap_or(Self::Danger),
            _ => Self::Danger,
        }
    }
}

/// Which tool calls are automatically approved without prompting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ApprovalMode {
    /// Read-only calls are auto-approved.
    #[default]
    Safe,
    /// Read-only and edit calls are auto-approved.
    Edits,
    /// All calls are auto-approved except shell commands that invoke git.
    Danger,
}

impl ApprovalMode {
    pub fn allows_call(&self, call: &ToolCall) -> bool {
        let model_safety = CommandSafety::from_tool_call(call);

        // Apply deterministic command policy on top of the model's self-classification.
        let effective_safety = if call.name == "bash" {
            if let Some(command) = call.arguments.get("command").and_then(Value::as_str) {
                let min_safety = minimum_required_classification(command);
                model_safety.max(min_safety)
            } else {
                model_safety
            }
        } else {
            model_safety
        };

        match self {
            Self::Safe => effective_safety == CommandSafety::ReadOnly,
            Self::Edits => matches!(
                effective_safety,
                CommandSafety::ReadOnly | CommandSafety::Edit
            ),
            Self::Danger => !is_git_bash_call(call),
        }
    }

    /// Cycle to the next mode: Safe → Edits → Danger → Safe.
    pub fn cycle(self) -> Self {
        match self {
            Self::Safe => Self::Edits,
            Self::Edits => Self::Danger,
            Self::Danger => Self::Safe,
        }
    }

    /// Short label for the status bar.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Safe => "Safe",
            Self::Edits => "Edits",
            Self::Danger => "Danger",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub name: String,
    pub content: String,
    pub is_error: bool,
}

/// Deterministic command policy: inspects the raw command string and returns the
/// minimum safety classification regardless of what the model claims.  This runs
/// *before* auto-approval so that a misclassified `rm -rf /` can never be treated
/// as read-only.
pub fn minimum_required_classification(command: &str) -> CommandSafety {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    if tokens.is_empty() {
        return CommandSafety::ReadOnly;
    }

    // ------------------------------------------------------------------
    // Force Danger — destructive, privileged, or system-modifying commands
    // ------------------------------------------------------------------

    const DANGER_COMMANDS: &[&str] = &[
        "rm", "rmdir", "chmod", "chown", "sudo", "mkfs", "dd", "shutdown", "reboot", "kill",
        "killall",
    ];
    for token in &tokens {
        if DANGER_COMMANDS.contains(token) {
            return CommandSafety::Danger;
        }
    }

    // systemctl with destructive subcommands
    for (i, token) in tokens.iter().enumerate() {
        if *token == "systemctl"
            && let Some(sub) = tokens.get(i + 1)
            && matches!(*sub, "stop" | "restart" | "disable" | "mask")
        {
            return CommandSafety::Danger;
        }
    }

    // service <name> stop|restart
    for (i, token) in tokens.iter().enumerate() {
        if *token == "service"
            && let Some(action) = tokens.get(i + 2)
            && matches!(*action, "stop" | "restart")
        {
            return CommandSafety::Danger;
        }
    }

    // curl / wget with -O / -o or any output redirection → network + filesystem write
    for token in &tokens {
        if (*token == "curl" || *token == "wget")
            && (command.contains(" -O") || command.contains(" -o ") || command.contains('>'))
        {
            return CommandSafety::Danger;
        }
    }

    // Redirections that write to absolute system paths (exclude /dev/ which is
    // harmless).
    if let Some(idx) = command.find("> /").or_else(|| command.find(">> /")) {
        let after = &command[idx..];
        if !after.starts_with("> /dev") && !after.starts_with(">> /dev") {
            return CommandSafety::Danger;
        }
    }

    // Destructive git commands — always prompt-worthy
    for (i, token) in tokens.iter().enumerate() {
        if *token == "git"
            && let Some(sub) = tokens.get(i + 1)
                && matches!(*sub, "push" | "reset" | "checkout" | "rebase" | "clean") {
                    return CommandSafety::Danger;
                }
    }

    // ------------------------------------------------------------------
    // Force Edit — filesystem / package mutations
    // ------------------------------------------------------------------

    const EDIT_COMMANDS: &[&str] = &["mv", "cp", "mkdir", "touch", "tee"];
    for token in &tokens {
        if EDIT_COMMANDS.contains(token) {
            return CommandSafety::Edit;
        }
    }

    // System-level package managers (install / remove / upgrade = mutation)
    const PKG_MANAGERS: &[&str] = &["apt", "apt-get", "yum", "dnf", "pacman", "brew"];
    for token in &tokens {
        if PKG_MANAGERS.contains(token) {
            return CommandSafety::Edit;
        }
    }

    // Language-specific package installers
    for (i, token) in tokens.iter().enumerate() {
        if (*token == "pip" || *token == "pip3" || *token == "npm")
            && tokens.get(i + 1) == Some(&"install")
        {
            return CommandSafety::Edit;
        }
        if *token == "cargo" && tokens.get(i + 1) == Some(&"install") {
            return CommandSafety::Edit;
        }
    }

    // Shell redirections (>) always imply writing somewhere
    if command.contains('>') || command.contains(">>") {
        return CommandSafety::Edit;
    }

    // Pipes to tee
    if command.contains("| tee") {
        return CommandSafety::Edit;
    }

    // sed -i (in-place file editing)
    for (i, token) in tokens.iter().enumerate() {
        if *token == "sed"
            && tokens
                .iter()
                .skip(i + 1)
                .any(|t| t.starts_with("-i") || *t == "--in-place")
            {
                return CommandSafety::Edit;
            }
    }

    // awk with potential file writes (either redirection or internal >)
    for token in &tokens {
        if *token == "awk" && command.contains('>') {
            return CommandSafety::Edit;
        }
    }

    // ------------------------------------------------------------------
    // ReadOnly allowlist — safe inspection commands
    // ------------------------------------------------------------------

    let first = tokens[0];

    const READONLY_COMMANDS: &[&str] = &[
        "ls", "pwd", "cat", "head", "tail", "rg", "grep", "find", "wc", "sort", "uniq", "echo",
        "which", "env", "printenv",
    ];
    if READONLY_COMMANDS.contains(&first) {
        return CommandSafety::ReadOnly;
    }

    // Cargo inspection / build commands (build produces artifacts but is
    // treated as "project workspace" read-only in the original model).
    if first == "cargo"
        && let Some(sub) = tokens.get(1)
            && matches!(*sub, "check" | "test" | "build") {
                return CommandSafety::ReadOnly;
            }

    // Git inspection commands
    if first == "git"
        && let Some(sub) = tokens.get(1)
            && matches!(*sub, "status" | "log" | "diff" | "branch") {
                return CommandSafety::ReadOnly;
            }

    // Version inquiries
    if matches!(first, "node" | "rustc" | "cargo") && tokens.get(1) == Some(&"--version") {
        return CommandSafety::ReadOnly;
    }

    // Anything not explicitly allowed nor forbidden is at least Edit.
    CommandSafety::Edit
}

fn is_git_bash_call(call: &ToolCall) -> bool {
    if call.name != "bash" {
        return false;
    }

    let Some(command) = call.arguments.get("command").and_then(Value::as_str) else {
        return false;
    };

    command
        .split(|ch: char| !matches!(ch, 'A'..='Z' | 'a'..='z' | '0'..='9' | '_' | '-' | '.'))
        .any(|token| token == "git")
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn definition(&self) -> ToolDefinition;
    async fn execute(&self, arguments: Value) -> Result<String, String>;
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{ApprovalMode, CommandSafety, ToolCall};

    fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "id".to_string(),
            name: name.to_string(),
            arguments,
        }
    }

    #[test]
    fn safe_mode_only_allows_read_only() {
        assert!(
            ApprovalMode::Safe.allows_call(&call("read_file", json!({ "path": "Cargo.toml" })))
        );
        assert!(ApprovalMode::Safe.allows_call(&call(
            "bash",
            json!({ "command": "pwd", "classification": "read_only" })
        )));
        assert!(
            !ApprovalMode::Safe.allows_call(&call("edit_file", json!({ "path": "Cargo.toml" })))
        );
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
    fn danger_mode_blocks_git_bash_commands() {
        assert!(ApprovalMode::Danger.allows_call(&call(
            "bash",
            json!({ "command": "rm -rf target", "classification": "danger" })
        )));
        assert!(!ApprovalMode::Danger.allows_call(&call(
            "bash",
            json!({ "command": "git status", "classification": "read_only" })
        )));
        assert!(!ApprovalMode::Danger.allows_call(&call(
            "bash",
            json!({ "command": "cd repo && git commit -am x", "classification": "danger" })
        )));
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

    use super::minimum_required_classification;

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
        // /dev/ redirections are harmless
        assert_eq!(
            minimum_required_classification("echo foo > /dev/null"),
            CommandSafety::Edit
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

        // Model says danger but ls is ReadOnly — policy doesn't downgrade
        let c5 = call(
            "bash",
            json!({ "command": "ls -la", "classification": "danger" }),
        );
        assert!(!ApprovalMode::Safe.allows_call(&c5)); // still blocked because model said danger
        assert!(!ApprovalMode::Edits.allows_call(&c5));
        assert!(ApprovalMode::Danger.allows_call(&c5));
    }

    /// The policy never downgrades; it only upgrades.
    #[test]
    fn policy_never_downgrades() {
        // git status is ReadOnly by policy, but if model says danger, it stays danger
        let effective = CommandSafety::from_tool_call(&call(
            "bash",
            json!({ "command": "git status", "classification": "danger" }),
        ))
        .max(minimum_required_classification("git status"));
        assert_eq!(effective, CommandSafety::Danger);

        // rm is Danger by policy, model says danger → stays danger
        let effective = CommandSafety::from_tool_call(&call(
            "bash",
            json!({ "command": "rm foo", "classification": "danger" }),
        ))
        .max(minimum_required_classification("rm foo"));
        assert_eq!(effective, CommandSafety::Danger);

        // ls is ReadOnly by policy, model says read_only → stays read_only
        let effective = CommandSafety::from_tool_call(&call(
            "bash",
            json!({ "command": "ls", "classification": "read_only" }),
        ))
        .max(minimum_required_classification("ls"));
        assert_eq!(effective, CommandSafety::ReadOnly);
    }
}

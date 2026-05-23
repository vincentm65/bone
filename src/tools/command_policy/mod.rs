use serde::Serialize;
use serde_json::Value;

use crate::tools::types::ToolCall;

/// Safety classification supplied with shell commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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
    pub fn rank(self) -> u8 {
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

    // Dangerous git commands — always prompt-worthy.
    if has_dangerous_git_command(&tokens) {
        return CommandSafety::Danger;
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

    // Shell redirections imply writing unless they only discard output.
    if has_non_dev_null_redirection(command) {
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
        "which", "env", "printenv", "date", "whoami", "id", "uname", "du", "df", "ps", "file",
        "stat", "realpath", "basename", "dirname", "tree",
    ];
    if READONLY_COMMANDS.contains(&first) {
        return CommandSafety::ReadOnly;
    }

    // Cargo inspection / build commands (build produces artifacts but is
    // treated as "project workspace" read-only in the original model).
    if first == "cargo"
        && let Some(sub) = tokens.get(1)
        && matches!(*sub, "check" | "test" | "build")
    {
        return CommandSafety::ReadOnly;
    }

    // Git inspection commands
    if first == "git"
        && let Some(sub) = tokens.get(1)
        && matches!(
            *sub,
            "status" | "log" | "diff" | "branch" | "show" | "ls-files"
        )
    {
        return CommandSafety::ReadOnly;
    }

    // Version inquiries
    if matches!(first, "node" | "rustc" | "cargo") && tokens.get(1) == Some(&"--version") {
        return CommandSafety::ReadOnly;
    }

    // Anything not explicitly allowed nor forbidden is at least Edit.
    CommandSafety::Edit
}

fn has_non_dev_null_redirection(command: &str) -> bool {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    tokens.iter().enumerate().any(|(i, token)| {
        let Some((_, target)) = token.split_once('>') else {
            return false;
        };
        let target = if target.is_empty() {
            tokens.get(i + 1).copied().unwrap_or_default()
        } else {
            target
        };
        target != "/dev/null"
    })
}

pub fn is_dangerous_git_bash_call(call: &ToolCall) -> bool {
    if call.name != "bash" {
        return false;
    }

    let Some(command) = call.arguments.get("command").and_then(Value::as_str) else {
        return false;
    };

    let tokens: Vec<&str> = command
        .split(|ch: char| !matches!(ch, 'A'..='Z' | 'a'..='z' | '0'..='9' | '_' | '-' | '.'))
        .collect();
    has_dangerous_git_command(&tokens)
}

fn has_dangerous_git_command(tokens: &[&str]) -> bool {
    tokens.windows(2).any(|pair| {
        pair[0] == "git"
            && matches!(
                pair[1],
                "push"
                    | "commit"
                    | "reset"
                    | "checkout"
                    | "switch"
                    | "restore"
                    | "rebase"
                    | "clean"
                    | "merge"
                    | "pull"
                    | "tag"
            )
    })
}

#[cfg(test)]
mod tests;

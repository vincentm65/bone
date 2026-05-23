use serde::Serialize;
use serde_json::Value;

use crate::tools::types::ToolCall;

/// Safety classification for shell commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandSafety {
    /// Read-only: does not modify files, services, network state, or git state.
    ReadOnly,
    /// Edit: creates, updates, or deletes project files, installs dependencies, etc.
    Edit,
    /// Danger: destructive, privileged, external side-effecting, or high-risk.
    Danger,
}

impl CommandSafety {
    /// Numeric rank (ReadOnly=0, Edit=1, Danger=2).
    pub fn rank(self) -> u8 {
        match self {
            Self::ReadOnly => 0,
            Self::Edit => 1,
            Self::Danger => 2,
        }
    }

    /// More restrictive (higher-ranked) classification.
    pub fn max(self, other: Self) -> Self {
        if self.rank() >= other.rank() {
            self
        } else {
            other
        }
    }

    /// Classify a tool call for approval. Model-provided `classification` on bash
    /// calls is ignored — policy is the sole authority.
    pub fn for_call(call: &ToolCall) -> Self {
        match call.name.as_str() {
            "read_file" => Self::ReadOnly,
            "write_file" | "edit_file" => Self::Edit,
            "bash" => call
                .arguments
                .get("command")
                .and_then(Value::as_str)
                .map(classify_command)
                .unwrap_or(Self::Danger),
            _ => Self::Danger,
        }
    }
}

/// Strip `bash -c` / `sh -c` wrappers so the inner command is classified.
fn peel_shell_wrapper(command: &str) -> &str {
    let trimmed = command.trim_start();
    let shells = ["bash", "sh", "zsh", "fish"];
    for shell in shells {
        if let Some(rest) = trimmed.strip_prefix(shell) {
            let rest = rest.trim_start();
            // Strip `-c` flag (with optional value that may follow)
            if let Some(after_c) = rest.strip_prefix("-c") {
                let after_c = after_c.trim_start();
                // `-c "cmd"` — strip one layer of quotes
                if let Some(inner) = after_c
                    .strip_prefix('"')
                    .and_then(|s| s.strip_suffix('"'))
                {
                    return inner;
                }
                if let Some(inner) = after_c
                    .strip_prefix('\'')
                    .and_then(|s| s.strip_suffix('\''))
                {
                    return inner;
                }
                return after_c;
            }
            return rest;
        }
    }
    command
}

/// Classify a command string based on policy only — never on model claims.
pub fn classify_command(command: &str) -> CommandSafety {
    // Peel off shell wrappers: `bash cmd args…`, `sh -c "cmd args…"`, etc.
    let command = peel_shell_wrapper(command);

    // Split on compound command operators (&&, ||, ;) and classify each
    // segment independently, taking the most restrictive result.
    let mut max = CommandSafety::ReadOnly;
    for segment in split_compound_commands(command) {
        let s = classify_segment(segment);
        if s.rank() > max.rank() {
            max = s;
            if max == CommandSafety::Danger {
                return max; // cannot get worse
            }
        }
    }
    max
}

fn classify_segment(command: &str) -> CommandSafety {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    if tokens.is_empty() {
        return CommandSafety::ReadOnly;
    }

    // -- Danger --

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

    // -- Edit --

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

    // -- ReadOnly --

    let first = tokens[0];

    const READONLY_COMMANDS: &[&str] = &[
        "cd", "ls", "pwd", "cat", "head", "tail", "rg", "grep", "find", "wc", "sort", "uniq",
        "echo", "which", "env", "printenv", "date", "whoami", "id", "uname", "du", "df", "ps",
        "file", "stat", "realpath", "basename", "dirname", "tree",
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

/// Split on `&&`, `||`, and `;`. Each segment passes policy independently.
fn split_compound_commands(command: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut start = 0;
    let bytes = command.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b';' {
            segments.push(command[start..i].trim());
            start = i + 1;
        } else if b == b'&' && bytes.get(i + 1) == Some(&b'&') {
            segments.push(command[start..i].trim());
            start = i + 2;
        } else if b == b'|' && bytes.get(i + 1) == Some(&b'|') {
            segments.push(command[start..i].trim());
            start = i + 2;
        }
    }
    let tail = command[start..].trim();
    if !tail.is_empty() {
        segments.push(tail);
    }
    segments
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



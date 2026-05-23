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

fn peel_shell_wrapper(command: &str) -> &str {
    let trimmed = command.trim_start();
    let shells = ["bash", "sh", "zsh", "fish"];
    for shell in shells {
        if let Some(rest) = trimmed.strip_prefix(shell) {
            let rest = rest.trim_start();
            if let Some(after_c) = rest.strip_prefix("-c") {
                let after_c = after_c.trim_start();
                if let Some(inner) = after_c.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
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
    shell_segments(peel_shell_wrapper(command))
        .into_iter()
        .map(|segment| classify_segment(&segment))
        .fold(CommandSafety::ReadOnly, |max, safety| max.max(safety))
}

fn classify_segment(command: &str) -> CommandSafety {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    if tokens.is_empty() {
        return CommandSafety::ReadOnly;
    }

    const DANGER_COMMANDS: &[&str] = &[
        "rm", "rmdir", "chmod", "chown", "sudo", "mkfs", "dd", "shutdown", "reboot", "kill",
        "killall",
    ];
    if tokens.iter().any(|token| DANGER_COMMANDS.contains(token)) {
        return CommandSafety::Danger;
    }

    for (i, token) in tokens.iter().enumerate() {
        if *token == "systemctl"
            && let Some(sub) = tokens.get(i + 1)
            && matches!(*sub, "stop" | "restart" | "disable" | "mask")
        {
            return CommandSafety::Danger;
        }
    }

    for (i, token) in tokens.iter().enumerate() {
        if *token == "service"
            && let Some(action) = tokens.get(i + 2)
            && matches!(*action, "stop" | "restart")
        {
            return CommandSafety::Danger;
        }
    }

    if tokens.iter().any(|token| matches!(*token, "curl" | "wget"))
        && (command.contains(" -O") || command.contains(" -o ") || command.contains('>'))
    {
        return CommandSafety::Danger;
    }

    if let Some(idx) = command.find("> /").or_else(|| command.find(">> /")) {
        let after = &command[idx..];
        if !after.starts_with("> /dev") && !after.starts_with(">> /dev") {
            return CommandSafety::Danger;
        }
    }

    if has_dangerous_git_command(&tokens) {
        return CommandSafety::Danger;
    }

    const EDIT_COMMANDS: &[&str] = &["mv", "cp", "mkdir", "touch", "tee"];
    if tokens.iter().any(|token| EDIT_COMMANDS.contains(token)) {
        return CommandSafety::Edit;
    }

    const PKG_MANAGERS: &[&str] = &["apt", "apt-get", "yum", "dnf", "pacman", "brew"];
    if tokens.iter().any(|token| PKG_MANAGERS.contains(token)) {
        return CommandSafety::Edit;
    }

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

    if has_non_dev_null_redirection(command) {
        return CommandSafety::Edit;
    }

    if command.contains("| tee") {
        return CommandSafety::Edit;
    }

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

    if tokens.contains(&"awk") && command.contains('>') {
        return CommandSafety::Edit;
    }

    let first = tokens[0];

    const READONLY_COMMANDS: &[&str] = &[
        "cd", "ls", "pwd", "cat", "head", "tail", "rg", "grep", "find", "xargs", "wc", "sort",
        "uniq", "echo", "which", "env", "printenv", "date", "whoami", "id", "uname", "du", "df",
        "ps", "file", "stat", "realpath", "basename", "dirname", "tree",
    ];
    if READONLY_COMMANDS.contains(&first) {
        return CommandSafety::ReadOnly;
    }

    // Build commands produce artifacts, but are treated as project-local and auto-approved.
    if first == "cargo"
        && let Some(sub) = tokens.get(1)
        && matches!(*sub, "check" | "test" | "build")
    {
        return CommandSafety::ReadOnly;
    }

    if first == "git"
        && let Some(sub) = tokens.get(1)
        && matches!(
            *sub,
            "status" | "log" | "diff" | "branch" | "show" | "ls-files"
        )
    {
        return CommandSafety::ReadOnly;
    }

    if matches!(first, "node" | "rustc" | "cargo") && tokens.get(1) == Some(&"--version") {
        return CommandSafety::ReadOnly;
    }

    CommandSafety::Edit
}

fn shell_segments(command: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut single = false;
    let mut double = false;
    let mut escaped = false;
    let mut at_word_start = true;
    let mut chars = command.chars().peekable();

    while let Some(ch) = chars.next() {
        if escaped {
            current.push(ch);
            escaped = false;
            at_word_start = ch.is_whitespace();
            continue;
        }
        if ch == '\\' {
            current.push(ch);
            escaped = true;
            at_word_start = false;
            continue;
        }
        if ch == '\'' && !double {
            single = !single;
            current.push(ch);
            at_word_start = false;
            continue;
        }
        if ch == '"' && !single {
            double = !double;
            current.push(ch);
            at_word_start = false;
            continue;
        }
        if ch == '#' && !single && !double && at_word_start {
            for next in chars.by_ref() {
                if next == '\n' {
                    push_segment(&mut segments, &mut current);
                    at_word_start = true;
                    break;
                }
            }
            continue;
        }
        if !single && !double {
            match ch {
                '&' if chars.peek() == Some(&'&') => {
                    chars.next();
                    push_segment(&mut segments, &mut current);
                    at_word_start = true;
                    continue;
                }
                '|' if chars.peek() == Some(&'|') => {
                    chars.next();
                    push_segment(&mut segments, &mut current);
                    at_word_start = true;
                    continue;
                }
                ';' | '|' | '\n' => {
                    push_segment(&mut segments, &mut current);
                    at_word_start = true;
                    continue;
                }
                _ => {}
            }
        }

        current.push(ch);
        at_word_start = ch.is_whitespace();
    }
    push_segment(&mut segments, &mut current);
    segments
}

fn push_segment(segments: &mut Vec<String>, segment: &mut String) {
    let trimmed = segment.trim();
    if !trimmed.is_empty() {
        segments.push(trimmed.to_string());
    }
    segment.clear();
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

//! Command safety classification and allow/deny policy enforcement.

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config;
use crate::tools::types::ToolCall;

const DEFAULT_COMMAND_POLICY: &str = include_str!("../../../default-command-policy.yaml");

/// Safety classification for shell commands and tools.
///
/// In safe mode only `ReadOnly` calls auto-run. In danger mode everything
/// auto-runs. There is no edit mode — anything that mutates state is `Danger`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandSafety {
    /// Read-only: does not modify files, services, network state, or git state.
    #[serde(alias = "safe")]
    ReadOnly,
    /// Danger: anything that creates, updates, deletes, installs, or has side effects.
    Danger,
}

impl CommandSafety {
    /// Numeric rank (ReadOnly=0, Danger=1).
    pub fn rank(self) -> u8 {
        match self {
            Self::ReadOnly => 0,
            Self::Danger => 1,
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

    /// Classify a tool call for approval. Model-provided `classification` on shell
    /// calls is ignored — policy is the sole authority.
    pub fn for_call(call: &ToolCall) -> Self {
        match call.name.as_str() {
            "read_file" => Self::ReadOnly,
            "write_file" | "edit_file" => Self::Danger,
            "shell" => call
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
    for shell in &command_policy().shell_wrappers {
        if let Some(rest) = strip_command_prefix(trimmed, shell) {
            let rest = rest.trim_start();
            return peel_shell_args(rest).unwrap_or(rest);
        }
    }
    command
}

fn strip_command_prefix<'a>(command: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = command.get(prefix.len()..)?;
    command[..prefix.len()]
        .eq_ignore_ascii_case(prefix)
        .then_some(rest)
        .filter(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
}

fn peel_shell_args(command: &str) -> Option<&str> {
    let mut rest = command.trim_start();
    while let Some(stripped) = rest.strip_prefix('-') {
        let flag_end = stripped.find(char::is_whitespace).unwrap_or(stripped.len());
        let flag = &stripped[..flag_end];
        rest = stripped[flag_end..].trim_start();
        if matches_ignore_ascii_case(flag, &["c", "command", "commandwithargs"]) {
            return Some(unquote(rest));
        }
        if matches_ignore_ascii_case(flag, &["noprofile", "noninteractive", "executionpolicy"]) {
            if flag.eq_ignore_ascii_case("executionpolicy") {
                rest = rest
                    .split_once(char::is_whitespace)
                    .map(|(_, tail)| tail.trim_start())
                    .unwrap_or("");
            }
            continue;
        }
        return None;
    }
    (!rest.is_empty()).then_some(rest)
}

fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| value.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(value)
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
    let names: Vec<String> = tokens.iter().map(|token| command_name(token)).collect();

    const DANGER_COMMANDS: &[&str] = &[
        "rm",
        "rmdir",
        "chmod",
        "chown",
        "sudo",
        "mkfs",
        "dd",
        "shutdown",
        "reboot",
        "kill",
        "killall",
        "remove-item",
        "del",
        "erase",
        "rd",
        "stop-process",
        "stop-service",
        "restart-computer",
        "stop-computer",
        "remove-itemproperty",
    ];
    if names
        .iter()
        .any(|token| contains_static_name(DANGER_COMMANDS, token))
    {
        return CommandSafety::Danger;
    }

    for (i, token) in names.iter().enumerate() {
        if token == "systemctl"
            && let Some(sub) = names.get(i + 1)
            && matches!(sub.as_str(), "stop" | "restart" | "disable" | "mask")
        {
            return CommandSafety::Danger;
        }
    }

    for (i, token) in names.iter().enumerate() {
        if token == "service"
            && let Some(action) = names.get(i + 2)
            && matches!(action.as_str(), "stop" | "restart")
        {
            return CommandSafety::Danger;
        }
    }

    if names
        .iter()
        .any(|token| matches!(token.as_str(), "systemctl" | "service"))
    {
        return CommandSafety::Danger;
    }

    if names
        .iter()
        .any(|token| matches!(token.as_str(), "curl" | "wget"))
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

    if has_dangerous_git_command(&names) {
        return CommandSafety::Danger;
    }

    let policy = command_policy();
    if names
        .iter()
        .any(|token| policy.danger.contains(token) || policy.package_managers.contains(token))
    {
        return CommandSafety::Danger;
    }

    for (i, token) in names.iter().enumerate() {
        if matches!(token.as_str(), "pip" | "pip3" | "npm")
            && names.get(i + 1).is_some_and(|sub| sub == "install")
        {
            return CommandSafety::Danger;
        }
        if token == "cargo" && names.get(i + 1).is_some_and(|sub| sub == "install") {
            return CommandSafety::Danger;
        }
    }

    if has_non_dev_null_redirection(command) {
        return CommandSafety::Danger;
    }

    if command.contains("| tee") {
        return CommandSafety::Danger;
    }

    for (i, token) in tokens.iter().enumerate() {
        if *token == "sed"
            && tokens
                .iter()
                .skip(i + 1)
                .any(|t| t.starts_with("-i") || *t == "--in-place")
        {
            return CommandSafety::Danger;
        }
    }

    if names.iter().any(|token| token == "awk") && command.contains('>') {
        return CommandSafety::Danger;
    }

    let first = names[0].as_str();

    if contains_config_name(&policy.read_only, first) {
        return CommandSafety::ReadOnly;
    }

    // Build commands produce artifacts, but are treated as project-local and auto-approved.
    if first == "cargo"
        && let Some(sub) = names.get(1)
        && matches!(sub.as_str(), "check" | "test" | "build")
    {
        return CommandSafety::ReadOnly;
    }

    if first == "git"
        && let Some(sub) = names.get(1)
        && matches!(
            sub.as_str(),
            "status" | "log" | "diff" | "branch" | "show" | "ls-files"
        )
    {
        return CommandSafety::ReadOnly;
    }

    if matches!(first, "node" | "rustc" | "cargo")
        && names.get(1).is_some_and(|arg| arg == "--version")
    {
        return CommandSafety::ReadOnly;
    }

    CommandSafety::Danger
}

fn shell_segments(command: &str) -> Vec<String> {
    crate::shell_split::shell_split(
        command,
        &crate::shell_split::ShellSplitOptions {
            keep_separators: false,
            split_newlines: true,
            strip_comments: true,
        },
    )
}
fn command_name(token: &str) -> String {
    let name = token
        .trim_matches(|ch: char| matches!(ch, '"' | '\'' | '&' | '(' | ')' | '{' | '}' | ','))
        .to_ascii_lowercase();
    name.strip_suffix(".exe").unwrap_or(&name).to_string()
}

fn matches_ignore_ascii_case(value: &str, candidates: &[&str]) -> bool {
    candidates
        .iter()
        .any(|candidate| value.eq_ignore_ascii_case(candidate))
}

#[derive(Debug, Deserialize)]
struct RawCommandPolicy {
    #[serde(default)]
    shell_wrappers: Vec<String>,
    #[serde(default)]
    read_only: Vec<String>,
    #[serde(default)]
    edit: Vec<String>,
    #[serde(default)]
    package_managers: Vec<String>,
}

#[derive(Debug)]
struct CommandPolicy {
    shell_wrappers: Vec<String>,
    read_only: Vec<String>,
    danger: Vec<String>,
    package_managers: Vec<String>,
}

fn command_policy() -> &'static CommandPolicy {
    static POLICY: OnceLock<CommandPolicy> = OnceLock::new();
    POLICY.get_or_init(load_command_policy)
}

fn load_command_policy() -> CommandPolicy {
    let path = config::command_policy_path();
    let raw = if path.exists() {
        config::load_yaml::<RawCommandPolicy>(&path).unwrap_or_else(|| {
            eprintln!("bone: warning: failed to parse {}", path.display());
            default_raw_command_policy()
        })
    } else {
        default_raw_command_policy()
    };

    // Merge edit + package_managers into danger (edit mode removed).
    let danger = raw
        .edit
        .into_iter()
        .chain(raw.package_managers.clone())
        .collect();

    CommandPolicy {
        shell_wrappers: normalize_shell_wrappers(raw.shell_wrappers),
        read_only: normalize_list(raw.read_only),
        danger: normalize_list(danger),
        package_managers: normalize_list(raw.package_managers),
    }
}

fn default_raw_command_policy() -> RawCommandPolicy {
    serde_yaml::from_str(DEFAULT_COMMAND_POLICY)
        .expect("bundled default-command-policy.yaml must be valid")
}

fn normalize_list(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .map(|value| command_name(&value))
        .filter(|value| !value.is_empty())
        .collect()
}

fn normalize_shell_wrappers(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect()
}

fn contains_static_name(names: &[&str], needle: &str) -> bool {
    names.contains(&needle)
}

fn contains_config_name(names: &[String], needle: &str) -> bool {
    names.iter().any(|name| name == needle)
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
        // Ignore fd-duplication redirects like 2>&1, 1>&2, etc.
        if target.starts_with('&') {
            return false;
        }
        target != "/dev/null"
    })
}

fn has_dangerous_git_command(tokens: &[String]) -> bool {
    tokens.windows(2).any(|pair| {
        pair[0] == "git"
            && matches!(
                pair[1].as_str(),
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

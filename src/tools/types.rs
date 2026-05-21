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
        let safety = CommandSafety::from_tool_call(call);
        match self {
            Self::Safe => safety == CommandSafety::ReadOnly,
            Self::Edits => matches!(safety, CommandSafety::ReadOnly | CommandSafety::Edit),
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
}

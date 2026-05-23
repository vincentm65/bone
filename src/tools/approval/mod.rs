use serde_json::Value;

use crate::tools::command_policy::{
    CommandSafety, is_dangerous_git_bash_call, minimum_required_classification,
};
use crate::tools::types::ToolCall;

/// Which tool calls are automatically approved without prompting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ApprovalMode {
    /// Read-only calls are auto-approved.
    #[default]
    Safe,
    /// Read-only and edit calls are auto-approved.
    Edits,
    /// All calls are auto-approved except dangerous git shell commands.
    Danger,
}

impl ApprovalMode {
    pub fn allows_call(&self, call: &ToolCall) -> bool {
        let effective_safety = if call.name == "bash" {
            call.arguments
                .get("command")
                .and_then(Value::as_str)
                .map(minimum_required_classification)
                .unwrap_or(CommandSafety::Danger)
        } else {
            CommandSafety::from_tool_call(call)
        };

        match self {
            Self::Safe => effective_safety == CommandSafety::ReadOnly,
            Self::Edits => matches!(
                effective_safety,
                CommandSafety::ReadOnly | CommandSafety::Edit
            ),
            Self::Danger => !is_dangerous_git_bash_call(call),
        }
    }

    /// Cycle to the next mode: Safe → Edit → Danger → Safe.
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
            Self::Edits => "Edit",
            Self::Danger => "Danger",
        }
    }
}

#[cfg(test)]
mod tests;

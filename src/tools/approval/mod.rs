use serde_json::Value;

use crate::tools::command_policy::{CommandSafety, is_git_bash_call, minimum_required_classification};
use crate::tools::types::ToolCall;

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
                model_safety.max(minimum_required_classification(command))
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

#[cfg(test)]
mod tests;

use crate::tools::command_policy::CommandSafety;
use crate::tools::types::ToolCall;

/// Which tool calls are automatically approved without prompting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ApprovalMode {
    /// Read-only calls are auto-approved.
    #[default]
    Safe,
    /// Read-only and edit calls are auto-approved.
    Edits,
    /// All calls are auto-approved.
    Danger,
}

impl ApprovalMode {
    pub fn allows_call(&self, call: &ToolCall) -> bool {
        let safety = CommandSafety::for_call(call);

        match self {
            Self::Safe => safety == CommandSafety::ReadOnly,
            Self::Edits => matches!(safety, CommandSafety::ReadOnly | CommandSafety::Edit),
            Self::Danger => true,
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

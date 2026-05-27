pub mod command_policy;
pub mod edit_file;
pub mod read_file;
pub mod registry;
pub mod shell;
pub mod types;
pub mod write_atomic;
pub mod write_file;

use registry::ToolRegistry;

pub use registry::ToolHandler;
pub use types::{ToolCall, ToolDefinition, ToolResult};

pub fn builtin_tools() -> ToolRegistry {
    ToolRegistry::new()
        .register(read_file::ReadFileTool)
        .register(write_file::WriteFileTool)
        .register(edit_file::EditFileTool)
        .register(shell::ShellTool)
}

// ── ApprovalMode ────────────────────────────────────────────────────────────

use command_policy::CommandSafety;
use serde::{Deserialize, Serialize};

/// Which tool calls are automatically approved without prompting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
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

    pub fn cycle(self) -> Self {
        match self {
            Self::Safe => Self::Edits,
            Self::Edits => Self::Danger,
            Self::Danger => Self::Safe,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Safe => "Safe",
            Self::Edits => "Edit",
            Self::Danger => "Danger",
        }
    }
}

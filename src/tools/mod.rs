pub mod command_policy;
pub mod approval;
pub mod edit_file;
pub mod read_file;
pub mod registry;
pub mod shell;
pub mod state_map;
pub mod types;
pub mod write_atomic;
pub mod write_file;

use registry::ToolRegistry;

use crate::ext::lua_tool::LuaTool;
pub use command_policy::CommandSafety;
pub use approval::{CallOutcome, decide_call, denied_message};
pub use shell::{ScriptOutput, ScriptRequest, run_script, truncate_output};
use std::collections::HashMap;
pub use types::{Tool, ToolCall, ToolDefinition, ToolResult};

/// Result of loading all tools (builtins + Lua) in a single pass.
pub struct LoadedTools {
    pub registry: ToolRegistry,
    /// Map from tool name to UI-only display metadata.
    pub dynamic_display: HashMap<String, types::ToolDisplayConfig>,
    /// Map from tool name to its declared safety level.
    pub dynamic_safety: HashMap<String, CommandSafety>,
}

pub fn load_tools() -> LoadedTools {
    let registry = builtin_tools();

    LoadedTools {
        registry,
        dynamic_display: HashMap::new(),
        dynamic_safety: HashMap::new(),
    }
}

/// Register Lua tools into an existing `LoadedTools`, respecting name conflict rules.
pub fn register_lua_tools(loaded: &mut LoadedTools, lua_tools: Vec<LuaTool>) {
    for tool in lua_tools {
        let name = tool.definition().name.clone();
        loaded
            .dynamic_display
            .insert(name.clone(), tool.display().clone());
        loaded.dynamic_safety.insert(name, tool.safety());
        loaded.registry = loaded.registry.clone().register(tool);
    }
}

pub fn builtin_tools() -> ToolRegistry {
    ToolRegistry::new()
        .register(read_file::ReadFileTool)
        .register(write_file::WriteFileTool)
        .register(edit_file::EditFileTool)
        .register(shell::ShellTool)
}

// ── ApprovalMode ────────────────────────────────────────────────────────────

use serde::{Deserialize, Serialize};

/// Which tool calls are automatically approved without prompting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalMode {
    /// Read-only calls are auto-approved.
    #[default]
    Safe,
    /// All calls are auto-approved.
    Danger,
}

impl ApprovalMode {
    pub fn allows_safety(&self, safety: CommandSafety) -> bool {
        match self {
            Self::Safe => safety == CommandSafety::ReadOnly,
            Self::Danger => true,
        }
    }

    pub fn allows_call(&self, call: &ToolCall) -> bool {
        let safety = CommandSafety::for_call(call);
        self.allows_safety(safety)
    }

    pub fn cycle(self) -> Self {
        match self {
            Self::Safe => Self::Danger,
            Self::Danger => Self::Safe,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Safe => "Safe",
            Self::Danger => "Danger",
        }
    }

    /// Lowercase short labels used in JSONL events.
    pub fn mode_str(&self) -> &'static str {
        match self {
            Self::Safe => "safe",
            Self::Danger => "danger",
        }
    }
}

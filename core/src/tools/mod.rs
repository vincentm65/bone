//! Built-in tools: registry, approval, command policy, and file/shell tools.

pub mod approval;
pub mod command_policy;
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
pub use approval::{
    ApprovalGate, AutoApprovalGate, CallOutcome, EscalatingGate, SharedGate, decide_call,
    denied_message,
};
pub use command_policy::CommandSafety;
pub use shell::{ScriptOutput, ScriptRequest, run_script, truncate_output};
use std::collections::HashMap;

/// Cap an individual line's length so a single minified multi-MB line can't
/// consume the whole context window. Truncates on a UTF-8 char boundary.
pub const MAX_TOOL_LINE_CHARS: usize = 2000;
pub fn truncate_line(line: &str) -> String {
    if line.chars().count() <= MAX_TOOL_LINE_CHARS {
        return line.to_string();
    }
    let end = line
        .char_indices()
        .nth(MAX_TOOL_LINE_CHARS)
        .map(|(offset, _)| offset)
        .unwrap_or(line.len());
    let mut out = line[..end].to_string();
    out.push_str("…[truncated]");
    out
}
pub use types::{Tool, ToolCall, ToolDefinition, ToolResult};

/// Result of loading all tools (builtins + Lua) in a single pass.
pub struct LoadedTools {
    pub registry: ToolRegistry,
    /// Map from tool name to UI-only display metadata.
    pub dynamic_display: HashMap<String, types::ToolDisplayConfig>,
    /// Map from tool name to its declared safety level.
    pub dynamic_safety: HashMap<String, CommandSafety>,
    /// Map from tool name to its host-held state key, for tools that declared
    /// `stateful = true`. Drives serialized execution and state threading.
    pub dynamic_state: HashMap<String, String>,
}

pub fn load_tools() -> LoadedTools {
    let registry = builtin_tools();

    LoadedTools {
        registry,
        dynamic_display: HashMap::new(),
        dynamic_safety: HashMap::new(),
        dynamic_state: HashMap::new(),
    }
}

/// Register Lua tools into an existing `LoadedTools`, respecting name conflict rules.
pub fn register_lua_tools(loaded: &mut LoadedTools, lua_tools: Vec<LuaTool>) {
    for tool in lua_tools {
        let name = tool.definition().name.clone();
        loaded
            .dynamic_display
            .insert(name.clone(), tool.display().clone());
        loaded.dynamic_safety.insert(name.clone(), tool.safety());
        if let Some(key) = tool.state_key() {
            loaded.dynamic_state.insert(name, key.to_string());
        }
        loaded.registry.register_mut(tool);
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

    /// Encode for storage in a shared [`std::sync::atomic::AtomicU8`] so a live
    /// frontend can toggle the mode while the driver is mid-turn (see
    /// [`SharedApprovalMode`]).
    pub fn as_u8(self) -> u8 {
        match self {
            Self::Safe => 0,
            Self::Danger => 1,
        }
    }

    /// Decode a value previously produced by [`Self::as_u8`]. Any unexpected
    /// value falls back to the safe default.
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Danger,
            _ => Self::Safe,
        }
    }
}

/// A handle to the current [`ApprovalMode`] that can be read and written from
/// multiple owners. The interactive frontend and the running [`Driver`] hold
/// clones of the *same* atomic, so cycling the mode mid-turn is observed by the
/// driver on its next tool batch.
///
/// This must stay interior-mutable: a plain `Arc<ApprovalMode>` cannot be
/// mutated once shared (`Arc::make_mut` forks a private copy instead), which
/// silently strands the driver on the old mode.
#[derive(Clone)]
pub struct SharedApprovalMode(std::sync::Arc<std::sync::atomic::AtomicU8>);

impl SharedApprovalMode {
    pub fn new(mode: ApprovalMode) -> Self {
        Self(std::sync::Arc::new(std::sync::atomic::AtomicU8::new(
            mode.as_u8(),
        )))
    }

    pub fn get(&self) -> ApprovalMode {
        ApprovalMode::from_u8(self.0.load(std::sync::atomic::Ordering::Relaxed))
    }

    pub fn set(&self, mode: ApprovalMode) {
        self.0
            .store(mode.as_u8(), std::sync::atomic::Ordering::Relaxed);
    }
}

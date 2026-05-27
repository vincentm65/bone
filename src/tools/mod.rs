pub mod command_policy;
pub mod dynamic;
pub mod edit_file;
pub mod read_file;
pub mod registry;
pub mod script_runner;
pub mod shell;
pub mod types;
pub mod write_atomic;
pub mod write_file;

use registry::ToolRegistry;

pub use dynamic::DynamicTool as DynamicToolType;
pub use registry::ToolHandler;
pub use types::{ToolCall, ToolDefinition, ToolResult};
use std::collections::HashMap;

/// Result of loading all tools (builtins + dynamic) in a single pass.
pub struct LoadedTools {
    pub registry: ToolRegistry,
    /// Names of dynamic tools that use `interaction: select`.
    pub interaction_tools: std::collections::HashSet<String>,
    /// Map from dynamic tool name to its script content (for display in approval).
    pub dynamic_scripts: HashMap<String, String>,
}

pub fn load_tools() -> LoadedTools {
    let mut registry = builtin_tools();
    let builtin_names: Vec<String> = registry
        .definitions()
        .iter()
        .map(|d| d.name.clone())
        .collect();

    // Load dynamic tools from disk (single parse)
    let dir = tools_dir();
    let _ = std::fs::create_dir_all(&dir);
    seed_default_tools(&dir);

    let dynamic = dynamic::load_from_dir(&dir);

    let mut interaction_tools = std::collections::HashSet::new();
    let mut dynamic_scripts = HashMap::new();

    for tool in dynamic {
        if builtin_names.contains(&tool.name) {
            eprintln!(
                "bone: warning: custom tool '{}' collides with builtin; skipping",
                tool.name
            );
            continue;
        }
        if matches!(tool.interaction, Some(dynamic::InteractionType::Select)) {
            interaction_tools.insert(tool.name.clone());
        }
        if let Some(ref script) = tool.script {
            dynamic_scripts.insert(tool.name.clone(), script.clone());
        }
        registry = registry.register(tool);
    }

    LoadedTools {
        registry,
        interaction_tools,
        dynamic_scripts,
    }
}

pub fn builtin_tools() -> ToolRegistry {
    ToolRegistry::new()
        .register(read_file::ReadFileTool)
        .register(write_file::WriteFileTool)
        .register(edit_file::EditFileTool)
        .register(shell::ShellTool)
}

pub fn tools_dir() -> std::path::PathBuf {
    crate::config::bone_dir().join("tools")
}

fn seed_default_tools(dir: &std::path::Path) {
    const DEFAULTS: &[(&str, &str)] = &[
        (
            "ask_user.yaml",
            include_str!("../../defaults/tools/ask_user.yaml"),
        ),
        (
            "web_search.yaml",
            include_str!("../../defaults/tools/web_search.yaml"),
        ),
    ];
    for (name, content) in DEFAULTS {
        let path = dir.join(name);
        if !path.exists() {
            if let Err(e) = std::fs::write(&path, content) {
                eprintln!("bone: warning: could not write {}: {e}", path.display());
            }
        }
    }
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

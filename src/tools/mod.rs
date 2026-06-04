pub mod command_policy;
pub mod dynamic;
pub mod edit_file;
pub mod read_file;
pub mod registry;
pub mod script_runner;
pub mod shell;
pub mod state_map;
pub mod types;
pub mod write_atomic;
pub mod write_file;

use registry::ToolRegistry;

pub use registry::ToolHandler;
use std::collections::HashMap;
include!(concat!(env!("OUT_DIR"), "/default_tools.rs"));
pub use types::{ToolCall, ToolDefinition, ToolResult};

/// Result of loading all tools (builtins + dynamic) in a single pass.
pub struct LoadedTools {
    pub registry: ToolRegistry,
    /// Names of dynamic tools that use `interaction: select`.
    pub interaction_tools: std::collections::HashSet<String>,
    /// Map from dynamic tool name to its script content (for display in approval).
    pub dynamic_scripts: HashMap<String, String>,
    /// Map from dynamic tool name to its declared safety level.
    pub dynamic_safety: HashMap<String, CommandSafety>,
    /// Map from dynamic tool name to UI-only display metadata.
    pub dynamic_display: HashMap<String, types::ToolDisplayConfig>,
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
    let mut dynamic_safety = HashMap::new();
    let mut dynamic_display = HashMap::new();

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
        let default_safety = CommandSafety::Danger;
        dynamic_safety.insert(tool.name.clone(), tool.safety.unwrap_or(default_safety));
        if let Some(display) = tool.display.clone() {
            dynamic_display.insert(tool.name.clone(), display);
        }
        registry = registry.register(tool);
    }

    LoadedTools {
        registry,
        interaction_tools,
        dynamic_scripts,
        dynamic_safety,
        dynamic_display,
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

pub fn default_dynamic_tool_names() -> Vec<&'static str> {
    DEFAULT_DYNAMIC_TOOLS
        .iter()
        .map(|(name, _, _)| *name)
        .collect()
}

pub fn seed_default_tools(dir: &std::path::Path) {
    for (_, name, content) in DEFAULT_DYNAMIC_TOOLS {
        let path = dir.join(name);
        seed_or_migrate_versioned_tool(&path, content);
    }
}

/// Seed bundled dynamic tools. Existing versioned files are never overwritten:
/// if a bundled default is newer, write `<tool>.yaml.new` and leave the user's
/// active file intact. Versionless legacy files are replaced because they predate
/// default-tool versioning and cannot be safely compared.
fn seed_or_migrate_versioned_tool(path: &std::path::Path, new_content: &str) {
    if !path.exists() {
        if let Err(e) = std::fs::write(path, new_content) {
            eprintln!("bone: warning: could not write {}: {e}", path.display());
        }
        return;
    }

    let new_version = tool_yaml_version(new_content).unwrap_or(0);
    if new_version == 0 {
        return;
    }

    let Ok(existing_raw) = std::fs::read_to_string(path) else {
        return;
    };
    let existing_version = tool_yaml_version(&existing_raw);
    if existing_version.is_some_and(|version| version >= new_version) {
        return;
    }

    if existing_version.is_some() {
        let candidate_path = path.with_extension("yaml.new");
        if !candidate_path.exists()
            && let Err(e) = std::fs::write(&candidate_path, new_content)
        {
            eprintln!(
                "bone: warning: could not write updated default {}: {e}",
                candidate_path.display()
            );
        }
        eprintln!(
            "bone: warning: {} is older than the bundled default; wrote {} and left the existing file unchanged",
            path.display(),
            candidate_path.display()
        );
        return;
    }

    if let Err(e) = std::fs::write(path, new_content) {
        eprintln!("bone: warning: could not update {}: {e}", path.display());
    }
}

fn tool_yaml_version(raw: &str) -> Option<u64> {
    serde_yaml::from_str::<serde_yaml::Value>(raw)
        .ok()
        .and_then(|v| v.get("version").and_then(|v| v.as_u64()))
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
    pub fn allows_safety(&self, safety: CommandSafety) -> bool {
        match self {
            Self::Safe => safety == CommandSafety::ReadOnly,
            Self::Edits => matches!(safety, CommandSafety::ReadOnly | CommandSafety::Edit),
            Self::Danger => true,
        }
    }

    pub fn allows_call(&self, call: &ToolCall) -> bool {
        let safety = CommandSafety::for_call(call);
        self.allows_safety(safety)
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

    /// Lowercase short labels used in sub-agent JSONL events.
    pub fn mode_str(&self) -> &'static str {
        match self {
            Self::Safe => "read_only",
            Self::Edits => "edit",
            Self::Danger => "danger",
        }
    }
}

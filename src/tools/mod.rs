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
use std::collections::HashMap;
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

pub fn seed_default_tools(dir: &std::path::Path) {
    const DEFAULTS: &[(&str, &str)] = &[
        (
            "ask_user.yaml",
            include_str!("../../defaults/tools/ask_user.yaml"),
        ),
        (
            "web_search.yaml",
            include_str!("../../defaults/tools/web_search.yaml"),
        ),
        (
            "subagent.yaml",
            include_str!("../../defaults/tools/subagent.yaml"),
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

    // Seed task_list.yaml (cross-platform, uses python3 -c via uv run).
    let task_list_content: &str = include_str!("../../defaults/tools/task_list.yaml");
    let task_list_path = dir.join("task_list.yaml");
    if !task_list_path.exists() {
        if let Err(e) = std::fs::write(&task_list_path, task_list_content) {
            eprintln!(
                "bone: warning: could not write {}: {e}",
                task_list_path.display()
            );
        }
    } else {
        migrate_task_list(&task_list_path, task_list_content);
    }

    clean_stale_task_dirs();
}

/// Migrate old task_list.yaml to the current version.
/// Overwrites if the version field is < 4.
fn migrate_task_list(path: &std::path::Path, new_content: &str) {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return;
    };
    let existing_version: u64 = serde_yaml::from_str::<serde_yaml::Value>(&raw)
        .ok()
        .and_then(|v| v.get("version").and_then(|v| v.as_u64()))
        .unwrap_or(0);
    if existing_version >= 4 {
        return; // current version or user-customized
    }
    if let Err(e) = std::fs::write(path, new_content) {
        eprintln!("bone: warning: could not update {}: {e}", path.display());
    }
}

/// Remove stale per-PID task directories left by previous bone instances.
fn clean_stale_task_dirs() {
    let tasks_dir = crate::config::bone_dir().join("tasks");
    let Ok(entries) = std::fs::read_dir(&tasks_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = match name.to_str() {
            Some(s) => s,
            None => continue,
        };
        // Only consider numeric directory names (PIDs)
        let Ok(pid) = name_str.parse::<u32>() else {
            continue;
        };
        // Skip our own PID
        if pid == std::process::id() {
            continue;
        }
        if !is_pid_alive(pid) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Check whether a process is still running.
fn is_pid_alive(pid: u32) -> bool {
    use std::process::Stdio;
    // On Unix, `kill -0 <pid>` checks existence without sending a signal.
    // On Windows, `tasklist /FI "PID eq <pid>"` does the same.
    if cfg!(unix) {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    } else {
        // Windows: check via tasklist
        let output = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .stdout(Stdio::piped())
            .output();
        match output {
            Ok(out) => String::from_utf8_lossy(&out.stdout).contains(&pid.to_string()),
            Err(_) => false,
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
}

//! Bone Lua extension system.
//!
//! Stage 00: embed Lua, load `init.lua`, expose logging.
//! Stage 02: Lua tool registration and execution.
//! Stage 03: Lua command registration and dispatch.

pub mod ctx;
mod engine;
pub mod jobs;
mod loader;
pub mod lua_tool;
pub mod ops_commands;
pub mod ops_events;
pub mod ops_plugins;
pub mod ops_tools;
pub mod snapshots;
pub mod types;

pub use types::{BootOptions, BootResult, BootedTools, EventDispatchResult, ExtensionManager};

include!(concat!(env!("OUT_DIR"), "/default_lua_tools.rs"));
include!(concat!(env!("OUT_DIR"), "/default_lua_commands.rs"));

use std::path::Path;

/// Boot the Lua extension system.
///
/// Creates the VM, populates the `bone` global table, executes
/// `~/.bone-rust/init.lua` if it exists, and collects registered tools.
/// Failures are logged but never crash the app.
pub fn boot(config_dir: &Path, cwd: &Path, opts: BootOptions) -> BootResult {
    loader::boot(config_dir, cwd, opts)
}

/// Full boot sequence: load tools, boot extensions, register Lua tools,
/// optionally sync the registry into `custom` (persisted), and build a
/// configured `ToolHandler`.
pub fn boot_with_tools(
    config_dir: &Path,
    cwd: &Path,
    custom: &mut super::config::custom::CustomConfigs,
    sync: bool,
    opts: BootOptions,
) -> BootedTools {
    let BootResult {
        manager: extensions,
        tools: lua_tools,
    } = boot(config_dir, cwd, opts);

    let mut loaded = super::tools::load_tools();
    super::tools::register_lua_tools(&mut loaded, lua_tools);

    let all_tool_names: Vec<String> = loaded
        .registry
        .definitions()
        .iter()
        .map(|d| d.name.clone())
        .collect();

    let enabled = if sync {
        custom.sync_tools_from_registry(&all_tool_names);
        let names = custom.enabled_tool_names();
        if names.is_empty() {
            all_tool_names
        } else {
            names
        }
    } else {
        all_tool_names
    };

    let tools = super::tools::registry::ToolHandler::with_enabled_safety_and_display(
        loaded.registry,
        &enabled,
        loaded.dynamic_display,
        loaded.dynamic_safety,
    );

    if sync {
        // Sync lua command names into the commands page.
        let all_command_names: Vec<String> = extensions
            .commands()
            .iter()
            .map(|c| c.name.clone())
            .collect();
        custom.sync_commands_from_list(&all_command_names);
    }

    BootedTools {
        manager: extensions,
        tools,
    }
}

/// Seed bundled default Lua tools into the config directory.
/// Existing files are never overwritten.
pub fn seed_default_lua_tools(dir: &Path) {
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("bone: warning: could not create {}: {e}", dir.display());
        return;
    }
    for (name, content) in DEFAULT_LUA_TOOLS {
        let path = dir.join(name);
        if !path.exists()
            && let Err(e) = std::fs::write(&path, content)
        {
            eprintln!("bone: warning: could not write {}: {e}", path.display());
        }
    }
}

/// Seed bundled default Lua commands into the config directory.
/// Existing files are never overwritten.
fn seed_default_lua_commands(dir: &Path) {
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("bone: warning: could not create {}: {e}", dir.display());
        return;
    }
    for (name, content) in DEFAULT_LUA_COMMANDS {
        let path = dir.join(name);
        if !path.exists()
            && let Err(e) = std::fs::write(&path, content)
        {
            eprintln!("bone: warning: could not write {}: {e}", path.display());
        }
    }
}

/// Execute all Lua files from a directory.
/// Files are expected to register tools, commands, or other extensions.
pub fn run_lua_files(lua: &mlua::Lua, dir: &std::path::Path) -> Result<(), String> {
    if !dir.is_dir() {
        return Ok(());
    }

    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("failed to read {}: {e}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "lua"))
        .collect();
    entries.sort();

    for path in entries {
        let source = std::fs::read_to_string(&path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        lua.load(&source)
            .set_name(path.file_name().unwrap().to_string_lossy().as_ref())
            .exec()
            .map_err(|e| format!("error executing {}: {e}", path.display()))?;
    }

    Ok(())
}

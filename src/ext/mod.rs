//! Bone Lua extension system.
//!
//! Stage 00: embed Lua, load `init.lua`, expose logging.
//! Stage 02: Lua tool registration and execution.
//! Stage 03: Lua command registration and dispatch.

pub mod api;
pub mod api_ui;
pub mod ctx;
mod engine;
pub mod inbox;
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
include!(concat!(env!("OUT_DIR"), "/default_lua_libs.rs"));

use std::path::Path;

fn should_refresh_seeded_lua(path: &Path, name: &str) -> bool {
    let Ok(existing) = std::fs::read_to_string(path) else {
        return false;
    };
    existing.contains("ctx.ui.interact")
        || (name == "ui/menu.lua" && !existing.contains("local function next_key"))
        || (name == "ui/menu.lua" && !existing.contains("split_leading_circle"))
}

/// Boot the Lua extension system.
///
/// Creates the VM, populates the `bone` global table, executes
/// `~/.bone-rust/init.lua` if it exists, and collects registered tools.
/// Failures are logged but never crash the app.
pub fn boot(
    config_dir: &Path,
    cwd: &Path,
    opts: BootOptions,
    model: &str,
    provider: &str,
) -> BootResult {
    loader::boot(config_dir, cwd, opts, model, provider)
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
    model: &str,
    provider: &str,
) -> BootedTools {
    let BootResult {
        manager: extensions,
        tools: lua_tools,
    } = boot(config_dir, cwd, opts, model, provider);

    let mut loaded = super::tools::load_tools();
    super::tools::register_lua_tools(&mut loaded, lua_tools);

    let all_tool_names: Vec<String> = loaded
        .registry
        .definitions()
        .iter()
        .map(|d| d.name.clone())
        .collect();

    let enabled = if sync {
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

    BootedTools {
        manager: extensions,
        tools,
    }
}

/// Seed bundled default Lua tools into the config directory.
/// Existing files are not overwritten except for bundled files that still use
/// the removed Rust interaction API.
pub fn seed_default_lua_tools(dir: &Path) {
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("bone: warning: could not create {}: {e}", dir.display());
        return;
    }
    for (name, content) in DEFAULT_LUA_TOOLS {
        let path = dir.join(name);
        if (!path.exists() || should_refresh_seeded_lua(&path, name))
            && let Err(e) = std::fs::write(&path, content)
        {
            eprintln!("bone: warning: could not write {}: {e}", path.display());
        }
    }
}

/// Seed bundled default Lua libraries into the config directory.
/// Existing files are not overwritten except for stale bundled menu modules
/// from the Rust-to-Lua menu migration.
pub fn seed_default_lua_libs(dir: &Path) {
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("bone: warning: could not create {}: {e}", dir.display());
        return;
    }
    for (name, content) in DEFAULT_LUA_LIBS {
        let path = dir.join(name);
        if let Some(parent) = path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            eprintln!("bone: warning: could not create {}: {e}", parent.display());
            continue;
        }
        if (!path.exists() || should_refresh_seeded_lua(&path, name))
            && let Err(e) = std::fs::write(&path, content)
        {
            eprintln!("bone: warning: could not write {}: {e}", path.display());
        }
    }
}

/// Seed bundled default Lua commands into the config directory.
/// Existing files are not overwritten except for bundled files that still use
/// the removed Rust interaction API.
fn seed_default_lua_commands(dir: &Path) {
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("bone: warning: could not create {}: {e}", dir.display());
        return;
    }
    for (name, content) in DEFAULT_LUA_COMMANDS {
        let path = dir.join(name);
        if (!path.exists() || should_refresh_seeded_lua(&path, name))
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

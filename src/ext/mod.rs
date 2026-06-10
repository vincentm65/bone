//! Bone Lua extension system.
//!
//! Stage 00: embed Lua, load `init.lua`, expose logging.
//! Stage 02: Lua tool registration and execution.
//! Stage 03: Lua command registration and dispatch.

pub mod ctx;
mod engine;
pub mod event;
mod loader;
pub mod lua_tool;
pub mod ops_commands;
pub mod ops_events;
pub mod ops_plugins;
pub mod ops_tools;
pub mod snapshots;
pub mod types;

pub use types::{BootResult, ExtensionManager};

include!(concat!(env!("OUT_DIR"), "/default_lua_tools.rs"));
include!(concat!(env!("OUT_DIR"), "/default_lua_commands.rs"));

use std::path::Path;

/// Boot the Lua extension system.
///
/// Creates the VM, populates the `bone` global table, executes
/// `~/.bone-rust/init.lua` if it exists, and collects registered tools.
/// Failures are logged but never crash the app.
pub fn boot(config_dir: &Path, cwd: &Path) -> BootResult {
    loader::boot(config_dir, cwd)
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
        if !path.exists() {
            if let Err(e) = std::fs::write(&path, content) {
                eprintln!("bone: warning: could not write {}: {e}", path.display());
            }
        }
    }
}

/// Return the config directory for Lua tools.
pub fn lua_tools_dir() -> std::path::PathBuf {
    crate::config::bone_dir().join("lua/tools")
}

/// Seed bundled default Lua commands into the config directory.
/// Existing files are never overwritten.
pub fn seed_default_lua_commands(dir: &Path) {
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("bone: warning: could not create {}: {e}", dir.display());
        return;
    }
    for (name, content) in DEFAULT_LUA_COMMANDS {
        let path = dir.join(name);
        if !path.exists() {
            if let Err(e) = std::fs::write(&path, content) {
                eprintln!("bone: warning: could not write {}: {e}", path.display());
            }
        }
    }
}

/// Lightweight Lua boot for headless/run mode.
/// Returns (lua_state, is_loaded) — same engine setup as `boot` but
/// without collecting tools or building the full ExtensionManager.
pub fn boot_lua(config_dir: &std::path::Path) -> Result<(mlua::Lua, bool), String> {
    let version = env!("CARGO_PKG_VERSION");
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let lua = engine::create_engine(&version, &std::path::PathBuf::from(&cwd), config_dir)?;

    let loaded = engine::run_init(&lua, config_dir)?;
    Ok((lua, loaded))
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

/// Execute all command files from the commands directory.
/// Each file is expected to call `bone.register_command()` to register itself.
pub fn run_default_commands(lua: &mlua::Lua, config_dir: &std::path::Path) -> Result<(), String> {
    run_lua_files(lua, &config_dir.join("lua/commands"))
}

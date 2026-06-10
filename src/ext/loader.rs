//! Loader — discovers and executes `init.lua` from the config directory,
//! then collects registered Lua tools.

use std::path::Path;
use std::sync::{Arc, Mutex};

use super::ctx::SharedState;
use super::engine;
use super::lua_tool::LuaTool;
use super::types::{BootResult, ExtensionManager};

/// Boot the Lua extension system.
///
/// 1. Creates the Lua VM with the `bone` global table.
/// 2. Executes `~/.bone-rust/init.lua` if it exists.
/// 3. Collects any tools registered via `bone.register_tool()`.
/// 4. Returns a `BootResult` owning the Lua VM and registered tools.
///
/// Errors during Lua construction or init.lua execution are logged and
/// the app continues without Lua support.
pub fn boot(config_dir: &Path, cwd: &Path) -> BootResult {
    let version = env!("CARGO_PKG_VERSION");
    let config_dir_str = config_dir.to_string_lossy().to_string();

    let lua = match engine::create_engine(version, cwd, config_dir) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("bone: warning: Lua engine creation failed: {e}");
            return BootResult {
                manager: ExtensionManager::from_arc(
                    Arc::new(Mutex::new(mlua::Lua::new())),
                    false,
                    Vec::new(),
                    super::snapshots::LuaConfigSnapshot::default(),
                    super::snapshots::LuaThemeSnapshot::default(),
                    super::snapshots::LuaKeymapSnapshot::default(),
                ),
                tools: Vec::new(),
                commands: Vec::new(),
                config_snapshot: super::snapshots::LuaConfigSnapshot::default(),
                theme_snapshot: super::snapshots::LuaThemeSnapshot::default(),
                keymap_snapshot: super::snapshots::LuaKeymapSnapshot::default(),
            };
        }
    };

    let loaded = match engine::run_init(&lua, config_dir) {
        Ok(loaded) => loaded,
        Err(e) => {
            eprintln!("bone: warning: init.lua failed: {e}");
            false
        }
    };

    // Seed default Lua tools and commands (never overwrite user files).
    super::seed_default_lua_tools(&config_dir.join("lua/tools"));
    super::seed_default_lua_commands(&config_dir.join("lua/commands"));

    // Run tool and command files from lua/{tools,commands}/ directories.
    if let Err(e) = super::run_lua_files(&lua, &config_dir.join("lua/tools")) {
        eprintln!("bone: warning: Lua tools failed: {e}");
    }
    if let Err(e) = super::run_lua_files(&lua, &config_dir.join("lua/commands")) {
        eprintln!("bone: warning: Lua commands failed: {e}");
    }

    // Shared mutable state for all Lua tools (ctx.state).
    let shared_state: SharedState = Arc::new(Mutex::new(std::collections::HashMap::new()));

    // Wrap the Lua in Arc<Mutex> so LuaTool and ExtensionManager share it.
    let lua_arc = Arc::new(Mutex::new(lua));

    // Collect registered tools from bone._tools.
    let tools = collect_tools(&lua_arc, &config_dir_str, &shared_state);

    let commands = collect_commands(&lua_arc);

    // Collect Lua config, theme, and keymap snapshots.
    let config_snapshot = collect_config_snapshot(&lua_arc);
    let theme_snapshot = collect_theme_snapshot(&lua_arc);
    let keymap_snapshot = collect_keymap_snapshot(&lua_arc);

    let manager = ExtensionManager::from_arc(
        lua_arc,
        loaded,
        commands.clone(),
        config_snapshot.clone(),
        theme_snapshot.clone(),
        keymap_snapshot.clone(),
    );
    BootResult {
        manager,
        tools,
        commands,
        config_snapshot,
        theme_snapshot,
        keymap_snapshot,
    }
}

/// Iterate `bone._tools` and build `LuaTool` instances.
fn collect_tools(
    lua_arc: &Arc<Mutex<mlua::Lua>>,
    config_dir: &str,
    shared_state: &SharedState,
) -> Vec<LuaTool> {
    let lua = match lua_arc.lock() {
        Ok(guard) => guard,
        Err(e) => {
            eprintln!("bone: warning: Lua mutex poisoned: {e}");
            return Vec::new();
        }
    };

    let bone_table = match lua.globals().get::<mlua::Table>("bone") {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };

    let tools_table = match bone_table.get::<mlua::Table>("_tools") {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };

    let mut tools = Vec::new();
    for entry in tools_table.sequence_values::<mlua::Table>() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        match LuaTool::from_entry(
            &lua,
            &entry,
            Arc::clone(lua_arc),
            config_dir.to_string(),
            shared_state.clone(),
        ) {
            Ok(tool) => tools.push(tool),
            Err(e) => eprintln!("bone: warning: {e}"),
        }
    }

    tools
}

/// Iterate `bone._commands` and build `RegisteredLuaCommand` instances.
fn collect_commands(
    lua_arc: &Arc<Mutex<mlua::Lua>>,
) -> Vec<super::ops_commands::RegisteredLuaCommand> {
    let lua = match lua_arc.lock() {
        Ok(guard) => guard,
        Err(e) => {
            eprintln!("bone: warning: Lua mutex poisoned: {e}");
            return Vec::new();
        }
    };

    let bone_table = match lua.globals().get::<mlua::Table>("bone") {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };

    let commands_table = match bone_table.get::<mlua::Table>("_commands") {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };

    let mut commands = Vec::new();
    for entry in commands_table.sequence_values::<mlua::Table>() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        let name: String = match entry.get::<mlua::Value>("name") {
            Ok(mlua::Value::String(s)) => match s.to_str() {
                Ok(s) => s.to_string(),
                Err(_) => {
                    eprintln!("bone: warning: command has invalid UTF-8; skipping");
                    continue;
                }
            },
            _ => {
                eprintln!("bone: warning: command entry missing name; skipping");
                continue;
            }
        };

        let description: String = entry
            .get::<mlua::Value>("description")
            .ok()
            .and_then(|v| {
                let s = v.as_string()?;
                let bs = s.to_str().ok()?;
                Some(bs.as_ref().to_string())
            })
            .unwrap_or_default();

        commands.push(super::ops_commands::RegisteredLuaCommand { name, description });
    }

    commands
}

/// Collect `bone.config` snapshot from Lua.
fn collect_config_snapshot(lua_arc: &Arc<Mutex<mlua::Lua>>) -> super::snapshots::LuaConfigSnapshot {
    let lua = match lua_arc.lock() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("bone: warning: Lua mutex poisoned: {e}");
            return super::snapshots::LuaConfigSnapshot::default();
        }
    };

    let bone_table = match lua.globals().get::<mlua::Table>("bone") {
        Ok(t) => t,
        Err(_) => return super::snapshots::LuaConfigSnapshot::default(),
    };

    match bone_table.get::<Option<mlua::Table>>("config") {
        Ok(Some(t)) => match super::snapshots::LuaConfigSnapshot::from_lua_table(&lua, &t) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("bone-lua warn: config snapshot failed: {e}");
                super::snapshots::LuaConfigSnapshot::default()
            }
        },
        _ => super::snapshots::LuaConfigSnapshot::default(),
    }
}

/// Collect `bone.theme` snapshot from Lua.
fn collect_theme_snapshot(lua_arc: &Arc<Mutex<mlua::Lua>>) -> super::snapshots::LuaThemeSnapshot {
    let lua = match lua_arc.lock() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("bone: warning: Lua mutex poisoned: {e}");
            return super::snapshots::LuaThemeSnapshot::default();
        }
    };

    let bone_table = match lua.globals().get::<mlua::Table>("bone") {
        Ok(t) => t,
        Err(_) => return super::snapshots::LuaThemeSnapshot::default(),
    };

    match bone_table.get::<Option<mlua::Table>>("theme") {
        Ok(Some(t)) => match super::snapshots::LuaThemeSnapshot::from_lua_table(&lua, &t) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("bone-lua warn: theme snapshot failed: {e}");
                super::snapshots::LuaThemeSnapshot::default()
            }
        },
        _ => super::snapshots::LuaThemeSnapshot::default(),
    }
}

/// Collect `bone.keymap` snapshot from Lua.
fn collect_keymap_snapshot(lua_arc: &Arc<Mutex<mlua::Lua>>) -> super::snapshots::LuaKeymapSnapshot {
    let lua = match lua_arc.lock() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("bone: warning: Lua mutex poisoned: {e}");
            return super::snapshots::LuaKeymapSnapshot::default();
        }
    };

    let bone_table = match lua.globals().get::<mlua::Table>("bone") {
        Ok(t) => t,
        Err(_) => return super::snapshots::LuaKeymapSnapshot::default(),
    };

    match bone_table.get::<Option<mlua::Table>>("keymap") {
        Ok(Some(t)) => match super::snapshots::LuaKeymapSnapshot::from_lua_table(&lua, &t) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("bone-lua warn: keymap snapshot failed: {e}");
                super::snapshots::LuaKeymapSnapshot::default()
            }
        },
        _ => super::snapshots::LuaKeymapSnapshot::default(),
    }
}

//! Loader — discovers and executes `init.lua` from the config directory,
//! then collects registered Lua tools.

use std::path::Path;
use std::sync::{Arc, Mutex};

use super::ctx::SharedState;
use super::engine;
use super::lua_tool::LuaTool;
use super::types::{BootOptions, BootResult, ExtensionManager};

/// Boot the Lua extension system.
///
/// 1. Creates the Lua VM with the `bone` global table.
/// 2. Executes `~/.bone-rust/init.lua` if it exists.
/// 3. Collects any tools registered via `bone.register_tool()`.
/// 4. Returns a `BootResult` owning the Lua VM and registered tools.
///
/// Errors during Lua construction or init.lua execution are logged and
/// the app continues without Lua support.
pub fn boot(
    config_dir: &Path,
    cwd: &Path,
    opts: BootOptions,
    model: &str,
    provider: &str,
) -> BootResult {
    let subagent = opts.agent_depth > 0;
    let version = env!("CARGO_PKG_VERSION");
    let config_dir_str = config_dir.to_string_lossy().to_string();
    // Standalone shared UI-state handle — lives outside the Lua VM mutex so the
    // TUI can drain diffs even while a tool blocks on ctx.ui.key().
    let shared_ui = super::api_ui::new_shared();

    let lua = match engine::create_engine(
        version,
        cwd,
        config_dir,
        opts,
        model,
        provider,
        shared_ui.clone(),
    ) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("bone: warning: Lua engine creation failed: {e}");
            return BootResult {
                manager: ExtensionManager::unloaded(),
                tools: Vec::new(),
                shared_state: crate::ext::ctx::new_shared_state(),
            };
        }
    };

    // Seed libraries before init.lua so user startup code can `require` them.
    if !subagent {
        super::seed_default_lua_libs(&config_dir.join("lua/lib"), None, false);
    }

    let loaded = match engine::run_init(&lua, config_dir) {
        Ok(loaded) => loaded,
        Err(e) => {
            eprintln!("bone: warning: init.lua failed: {e}");
            false
        }
    };

    // Seed default Lua tools and commands (never overwrite user files).
    // A persisted setup selection (from the onboarding wizard) narrows which
    // bundled tools/commands get seeded; absent it, all are seeded.
    let selection = crate::config::load_setup_selection();
    let tool_allow = selection
        .as_ref()
        .map(crate::config::SetupSelection::tool_set);
    let cmd_allow = selection
        .as_ref()
        .map(crate::config::SetupSelection::command_set);
    if !subagent {
        super::seed_default_lua_tools(&config_dir.join("lua/tools"), tool_allow.as_ref(), false);
        super::seed_default_lua_commands(
            &config_dir.join("lua/commands"),
            cmd_allow.as_ref(),
            false,
        );
    }

    // Run tool and command files from lua/{tools,commands}/ directories. The
    // onboarding selection is enforced here too, not just at seed time: a
    // previously seeded bundled file the user later deselected stays on disk
    // but must not load.
    if let Err(e) =
        super::run_lua_tool_files(&lua, &config_dir.join("lua/tools"), tool_allow.as_ref())
    {
        eprintln!("bone: warning: Lua tools failed: {e}");
    }
    if !subagent
        && let Err(e) =
            super::run_lua_command_files(&lua, &config_dir.join("lua/commands"), cmd_allow.as_ref())
    {
        eprintln!("bone: warning: Lua commands failed: {e}");
    }

    // Conversation-scoped ctx.state map: one Arc per boot so concurrent session
    // actors and subagent boots never share checklist / host tool state.
    let shared_state: SharedState = crate::ext::ctx::new_shared_state();

    // Wrap the Lua in Arc<Mutex> so LuaTool and ExtensionManager share it.
    let lua_arc = Arc::new(Mutex::new(lua));

    // Collect registered tools from bone._tools.
    let tools = collect_tools(&lua_arc, &config_dir_str, &shared_state, &shared_ui);

    let commands = collect_commands(&lua_arc);

    // Collect Lua config, theme, and keymap snapshots.
    let config_snapshot = collect_config_snapshot(&lua_arc);
    let theme_snapshot = collect_theme_snapshot(&lua_arc);
    let keymap_snapshot = collect_keymap_snapshot(&lua_arc);

    let manager = ExtensionManager::from_arc(
        lua_arc,
        true, // engine_ok
        loaded,
        commands,
        config_snapshot,
        theme_snapshot,
        keymap_snapshot,
        shared_ui,
    );
    BootResult {
        manager,
        tools,
        shared_state,
    }
}

/// Get the `bone` global table from the Lua VM.
/// Returns None if the bone table doesn't exist.
fn get_bone(lua: &mlua::Lua) -> Option<mlua::Table> {
    lua.globals().get("bone").ok()
}

/// Lock the Lua mutex and retrieve the `bone` global table.
/// Returns `T::default()` on mutex poison or missing bone table.
fn with_bone<T: Default>(
    lua_arc: &Arc<Mutex<mlua::Lua>>,
    f: impl FnOnce(&mlua::Lua, &mlua::Table) -> T,
) -> T {
    let lua = match lua_arc.lock() {
        Ok(guard) => guard,
        Err(e) => {
            eprintln!("bone: warning: Lua mutex poisoned: {e}");
            return T::default();
        }
    };

    let bone_table = match get_bone(&lua) {
        Some(t) => t,
        None => return T::default(),
    };

    f(&lua, &bone_table)
}

/// Iterate `bone._tools` and build `LuaTool` instances.
fn collect_tools(
    lua_arc: &Arc<Mutex<mlua::Lua>>,
    config_dir: &str,
    shared_state: &SharedState,
    shared_ui: &super::api_ui::SharedUi,
) -> Vec<LuaTool> {
    with_bone(lua_arc, |lua, bone_table| {
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
                lua,
                &entry,
                Arc::clone(lua_arc),
                config_dir.to_string(),
                shared_state.clone(),
                shared_ui.clone(),
            ) {
                Ok(tool) => tools.push(tool),
                Err(e) => eprintln!("bone: warning: {e}"),
            }
        }

        tools
    })
}

/// Iterate `bone._commands` and build `RegisteredLuaCommand` instances.
fn collect_commands(
    lua_arc: &Arc<Mutex<mlua::Lua>>,
) -> Vec<super::ops_commands::RegisteredLuaCommand> {
    with_bone(lua_arc, |_lua, bone_table| {
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
    })
}

/// Collect `bone.config` snapshot from Lua.
fn collect_config_snapshot(lua_arc: &Arc<Mutex<mlua::Lua>>) -> super::snapshots::LuaConfigSnapshot {
    with_bone(lua_arc, |lua, bone_table| {
        let mut snapshot = match bone_table.get::<Option<mlua::Table>>("config") {
            Ok(Some(t)) => match super::snapshots::LuaConfigSnapshot::from_lua_table(lua, &t) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("bone-lua warn: config snapshot failed: {e}");
                    super::snapshots::LuaConfigSnapshot::default()
                }
            },
            _ => super::snapshots::LuaConfigSnapshot::default(),
        };

        // Spinner/text presets come from the seeded ui.spinners lib, not bone.config.
        let (spinners, texts) = super::snapshots::collect_presets(lua);
        snapshot.spinners = spinners;
        snapshot.texts = texts;
        snapshot
    })
}

/// Collect `bone.theme` snapshot from Lua.
fn collect_theme_snapshot(lua_arc: &Arc<Mutex<mlua::Lua>>) -> super::snapshots::LuaThemeSnapshot {
    with_bone(lua_arc, |lua, bone_table| {
        match bone_table.get::<Option<mlua::Table>>("theme") {
            Ok(Some(t)) => match super::snapshots::LuaThemeSnapshot::from_lua_table(lua, &t) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("bone-lua warn: theme snapshot failed: {e}");
                    super::snapshots::LuaThemeSnapshot::default()
                }
            },
            _ => super::snapshots::LuaThemeSnapshot::default(),
        }
    })
}

/// Collect `bone.keymap` snapshot from Lua.
fn collect_keymap_snapshot(lua_arc: &Arc<Mutex<mlua::Lua>>) -> super::snapshots::LuaKeymapSnapshot {
    with_bone(lua_arc, |lua, bone_table| {
        match bone_table.get::<Option<mlua::Table>>("keymap") {
            Ok(Some(t)) => match super::snapshots::LuaKeymapSnapshot::from_lua_table(lua, &t) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("bone-lua warn: keymap snapshot failed: {e}");
                    super::snapshots::LuaKeymapSnapshot::default()
                }
            },
            _ => super::snapshots::LuaKeymapSnapshot::default(),
        }
    })
}

#[cfg(test)]
#[path = "loader_tests.rs"]
mod loader_tests;

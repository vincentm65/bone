//! Loader — discovers and executes `init.lua` from the config directory,
//! then collects registered Lua tools.

use std::path::Path;
use std::sync::{Arc, Mutex};

use super::ctx::SharedState;
use super::engine;
use super::lua_tool::LuaTool;
use super::types::{BootOptions, BootResult, ExtensionManager};
use crate::config::settings::Settings;

fn log_boot_warning(config_dir: &Path, message: impl std::fmt::Display) {
    super::ctx::lua_log(&config_dir.to_string_lossy(), "warn", &message.to_string());
}

/// Boot the Lua extension system.
///
/// 1. Creates the Lua VM with the `bone` global table.
/// 2. Executes `~/.bone-rust/init.lua` if it exists.
/// 3. Collects any tools registered via `bone.tool.register()`.
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
    shared_settings: Option<Arc<Mutex<Settings>>>,
) -> BootResult {
    let subagent = opts.agent_depth > 0;
    let version = env!("CARGO_PKG_VERSION");
    let config_dir_str = config_dir.to_string_lossy().to_string();
    // Standalone shared UI-state handle — lives outside the Lua VM mutex so the
    // TUI can drain diffs even while a tool blocks on ctx.ui.key().
    let shared_ui = super::api_ui::new_shared();

    // Load the canonical resolved settings unless the daemon supplied its
    // process-wide service shared by all conversation actors.
    let settings_arc = if let Some(settings) = shared_settings {
        settings
    } else {
        let settings = match Settings::load() {
            Ok(Some(s)) => s,
            Ok(None) => {
                let s = Settings::migrate_from_pages(&[]);
                if let Err(e) = s.save() {
                    log_boot_warning(
                        config_dir,
                        format_args!("could not write canonical settings: {e}"),
                    );
                }
                s
            }
            Err(e) => {
                log_boot_warning(
                    config_dir,
                    format_args!("could not load canonical settings: {e}"),
                );
                Settings::defaults()
            }
        };
        Arc::new(Mutex::new(settings))
    };
    let settings_registry = Arc::new(std::sync::RwLock::new(
        super::settings_registry::SettingsRegistry::default(),
    ));

    let lua = match engine::create_engine(
        version,
        cwd,
        config_dir,
        opts,
        model,
        provider,
        shared_ui.clone(),
        settings_arc.clone(),
        settings_registry.clone(),
    ) {
        Ok(l) => l,
        Err(e) => {
            log_boot_warning(config_dir, format_args!("Lua engine creation failed: {e}"));
            return BootResult {
                manager: ExtensionManager::unloaded(),
                tools: Vec::new(),
                shared_state: crate::ext::ctx::new_shared_state(),
            };
        }
    };

    if !subagent {
        let settings = settings_arc
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .resolved()
            .clone();
        if let Err(e) = super::ops_tools::register_config_subagents(&lua, &settings) {
            log_boot_warning(
                config_dir,
                format_args!("could not load configured sub-agents: {e}"),
            );
        }
    }

    // Seed libraries before init.lua so user startup code can `require` them.
    if !subagent {
        super::seed_default_lua_libs(&config_dir.join("lua/lib"), None, false);
    }

    let loaded = match engine::run_init(&lua, config_dir) {
        Ok(loaded) => loaded,
        Err(e) => {
            log_boot_warning(config_dir, format_args!("init.lua failed: {e}"));
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
    let mut fatal_loader_failure = false;
    if let Err(e) =
        super::run_lua_tool_files(&lua, &config_dir.join("lua/tools"), tool_allow.as_ref())
    {
        log_boot_warning(config_dir, format_args!("Lua tools failed: {e}"));
        fatal_loader_failure = true;
    }
    if !subagent
        && let Err(e) =
            super::run_lua_command_files(&lua, &config_dir.join("lua/commands"), cmd_allow.as_ref())
    {
        log_boot_warning(config_dir, format_args!("Lua commands failed: {e}"));
        fatal_loader_failure = true;
    }
    if fatal_loader_failure {
        return BootResult {
            manager: ExtensionManager::unloaded(),
            tools: Vec::new(),
            shared_state: crate::ext::ctx::new_shared_state(),
        };
    }

    // Conversation-scoped ctx.state map: one Arc per boot so concurrent session
    // actors and subagent boots never share checklist / host tool state.
    let shared_state: SharedState = crate::ext::ctx::new_shared_state();

    // Wrap the Lua in Arc<Mutex> so LuaTool and ExtensionManager share it.
    let lua_arc = Arc::new(Mutex::new(lua));

    // Collect registered tools from bone._tools.
    let tools = collect_tools(&lua_arc, &config_dir_str, &shared_state, &shared_ui);

    let commands = collect_commands(&lua_arc);

    let manager = ExtensionManager::from_arc(
        lua_arc,
        true, // engine_ok
        loaded,
        commands,
        settings_arc,
        settings_registry,
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
            super::ctx::runtime_warn(format!("bone: warning: Lua mutex poisoned: {e}"));
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
                Err(e) => super::ctx::runtime_warn_once(format!("bone: warning: {e}")),
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
                        super::ctx::runtime_warn_once(
                            "bone: warning: command has invalid UTF-8; skipping",
                        );
                        continue;
                    }
                },
                _ => {
                    super::ctx::runtime_warn_once(
                        "bone: warning: command entry missing name; skipping",
                    );
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

#[cfg(test)]
#[path = "loader_tests.rs"]
mod loader_tests;

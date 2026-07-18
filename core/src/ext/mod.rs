//! Bone Lua extension system.
//!
//! Stage 00: embed Lua, load `init.lua`, expose logging.
//! Stage 02: Lua tool registration and execution.
//! Stage 03: Lua command registration and dispatch.

pub mod api;
pub mod api_ui;
pub mod catalog;
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

pub use engine::{blank_init_lua, populated_init_lua};
pub use types::{BootOptions, BootResult, BootedTools, EventDispatchResult, ExtensionManager};

include!(concat!(env!("OUT_DIR"), "/default_lua_tools.rs"));
include!(concat!(env!("OUT_DIR"), "/default_lua_commands.rs"));
include!(concat!(env!("OUT_DIR"), "/default_lua_libs.rs"));

use std::collections::HashSet;
use std::path::{Component, Path};

fn is_safe_leaf_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains(['/', '\\', '\0'])
        && matches!(
            Path::new(name).components().collect::<Vec<_>>().as_slice(),
            [Component::Normal(_)]
        )
}

/// Extract a one-line description from bundled default Lua content for the
/// setup wizard's pickers. Prefers a `description = "..."` field (as used by
/// `register_tool`/`register_command`), then falls back to the first `--`
/// comment line, then an empty string.
fn extract_description(content: &str) -> String {
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed
            .split_once("description")
            .and_then(|(_, r)| r.trim_start().strip_prefix('='))
        {
            let rest = rest.trim();
            if let Some(stripped) = rest.strip_prefix('"')
                && let Some(end) = stripped.find('"')
            {
                return stripped[..end].to_string();
            }
        }
    }
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("--") {
            let rest = rest.trim_start_matches('-').trim();
            if !rest.is_empty() {
                return rest.to_string();
            }
        }
    }
    String::new()
}

fn catalog(items: &[(&'static str, &'static str)]) -> Vec<(&'static str, String)> {
    items
        .iter()
        .map(|(name, content)| (*name, extract_description(content)))
        .collect()
}

/// `(filename, description)` for every bundled default command.
pub fn default_command_catalog() -> Vec<(&'static str, String)> {
    catalog(DEFAULT_LUA_COMMANDS)
}

fn should_refresh_seeded_lua(path: &Path, name: &str) -> std::io::Result<bool> {
    let existing = std::fs::read_to_string(path)?;
    Ok(existing.contains("ctx.ui.interact")
        // Refresh bundled extensions that use the pre-namespace registration API.
        || existing.contains("bone.register_tool")
        || existing.contains("bone.register_command")
        // Refresh menus predating the pane migration or current option-row styling.
        || (name == "ui/menu.lua"
            && (!existing.contains("require(\"ui.pane\")")
                || !existing.contains("SELECTED_BG")
                || !existing.contains("description_spans")
                || !existing.contains("label_modifiers")
                || !existing.contains("initial_checked")))
        // History now includes aggregate message and token counts/status,
        // and lists via a candidate-first CTE instead of a full messages join.
        || (name == "history.lua"
            && (!existing.contains("total_token_count")
                || !existing.contains("WITH recent AS")))
        // task_list: refresh copies missing conversation-deduped reminders,
        // complete, or the low-friction advance action / prior-step auto-close.
        || (name == "task_list.lua"
            && (!existing.contains("emit_turn_message_once")
                || !existing.contains("if action == \"complete\" then")
                || !existing.contains("action == \"advance\"")
                || !existing.contains("close_prior_to_in_progress")))
        // subagent's eager-render + dispatch label moved from hardcoded host
        // special-casing to declared `display.eager` / `display.template`;
        // refresh older seeded copies that predate those fields.
        || (name == "subagent.lua" && !existing.contains("eager"))
        // config's providers page replaced the "Edit provider..." menu row with
        // an `[e] edit` action key; refresh older seeded copies that predate the
        // `action_keys` wiring so `e` opens the provider editor again.
        || (name == "config.lua" && !existing.contains("action_keys")))
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
    // Per-agent tool allowlist, captured before `opts` is moved into `boot`.
    let tool_allowlist = opts.tool_allowlist.clone();
    let BootResult {
        manager: extensions,
        tools: lua_tools,
        shared_state,
    } = boot(config_dir, cwd, opts, model, provider);

    let mut loaded = super::tools::load_tools();
    super::tools::register_lua_tools(&mut loaded, lua_tools);

    let all_tool_names: Vec<String> = loaded
        .registry
        .definitions()
        .iter()
        .map(|d| d.name.clone())
        .collect();

    let mut enabled = if sync {
        let names = custom.enabled_tool_names();
        if names.is_empty() {
            all_tool_names
        } else {
            names
        }
    } else {
        all_tool_names
    };

    // A per-agent allowlist further narrows the enabled set: a sub-agent only
    // sees tools that are both globally enabled and named in its allowlist.
    if let Some(allow) = &tool_allowlist {
        enabled.retain(|name| allow.contains(name));
    }

    let tools = super::tools::registry::ToolHandler::with_enabled_safety_and_display(
        loaded.registry,
        &enabled,
        loaded.dynamic_display,
        loaded.dynamic_safety,
        loaded.dynamic_state,
    )
    .with_working_dir(cwd)
    // Same Arc the Lua tools captured at collect time, so ctx.state is one map.
    .with_shared_state(shared_state);

    BootedTools {
        manager: extensions,
        tools,
    }
}

/// Seed bundled default Lua files from `bundled` into `dir`.
///
/// Creates `dir` (and each file's parent) as needed. `allow == Some(set)` seeds
/// only the named files; `None` seeds all. `force` unconditionally overwrites.
/// Existing files are refreshed when [`should_refresh_seeded_lua`] says so
/// (e.g. they still use a removed Rust interaction API).
fn seed_default_lua(
    dir: &Path,
    bundled: &[(&'static str, &'static str)],
    allow: Option<&HashSet<String>>,
    force: bool,
) {
    if let Err(e) = std::fs::create_dir_all(dir) {
        ctx::runtime_warn(format!(
            "bone: warning: could not create {}: {e}",
            dir.display()
        ));
        return;
    }
    for (name, content) in bundled {
        if let Some(allow) = allow
            && !allow.contains(*name)
        {
            continue;
        }
        let path = dir.join(name);
        if let Some(parent) = path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            ctx::runtime_warn(format!(
                "bone: warning: could not create {}: {e}",
                parent.display()
            ));
            continue;
        }
        let refresh = if force || !path.exists() {
            true
        } else {
            match should_refresh_seeded_lua(&path, name) {
                Ok(refresh) => refresh,
                Err(e) => {
                    ctx::runtime_warn(format!(
                        "bone: warning: could not inspect {}; preserving it: {e}",
                        path.display()
                    ));
                    false
                }
            }
        };
        if refresh {
            let permissions = std::fs::metadata(&path).ok().map(|meta| meta.permissions());
            if let Err(e) = crate::tools::write_atomic::write_atomic_sync(
                &path,
                content.as_bytes(),
                permissions,
            ) {
                ctx::runtime_warn(format!(
                    "bone: warning: could not write {}: {e}",
                    path.display()
                ));
            }
        }
    }
}

/// Seed bundled default Lua tools. See [`seed_default_lua`].
pub fn seed_default_lua_tools(dir: &Path, allow: Option<&HashSet<String>>, force: bool) {
    seed_default_lua(dir, DEFAULT_LUA_TOOLS, allow, force)
}

/// Seed bundled default Lua libraries. See [`seed_default_lua`].
pub fn seed_default_lua_libs(dir: &Path, allow: Option<&HashSet<String>>, force: bool) {
    seed_default_lua(dir, DEFAULT_LUA_LIBS, allow, force)
}

/// Seed bundled default Lua commands. See [`seed_default_lua`].
pub fn seed_default_lua_commands(dir: &Path, allow: Option<&HashSet<String>>, force: bool) {
    seed_default_lua(dir, DEFAULT_LUA_COMMANDS, allow, force)
}

/// Execute the Lua tool files from `dir`, honoring the onboarding selection.
/// Bundled default tools the user deselected are skipped; user-authored files
/// (not among the bundled defaults) always run. `allow == None` runs every
/// file (default / upgrade behavior).
pub fn run_lua_tool_files(
    lua: &mlua::Lua,
    dir: &std::path::Path,
    allow: Option<&HashSet<String>>,
) -> Result<(), String> {
    run_lua_files_selected(lua, dir, DEFAULT_LUA_TOOLS, allow)
}

/// Execute the Lua command files from `dir`, honoring the onboarding selection.
/// See [`run_lua_tool_files`] for semantics.
pub fn run_lua_command_files(
    lua: &mlua::Lua,
    dir: &std::path::Path,
    allow: Option<&HashSet<String>>,
) -> Result<(), String> {
    run_lua_files_selected(lua, dir, DEFAULT_LUA_COMMANDS, allow)
}

fn run_lua_files_selected(
    lua: &mlua::Lua,
    dir: &std::path::Path,
    bundled: &[(&'static str, &'static str)],
    allow: Option<&HashSet<String>>,
) -> Result<(), String> {
    let bundled_names: HashSet<&str> = bundled.iter().map(|(n, _)| *n).collect();
    run_lua_files_filtered(lua, dir, |name| match allow {
        // A deselected bundled default is skipped; anything not bundled (a
        // user's own file) always loads, as does everything when no selection
        // is persisted.
        Some(allow) if bundled_names.contains(name) => allow.contains(name),
        _ => true,
    })
}

/// Execute the `.lua` files in `dir` (sorted) for which `keep(file_name)` is
/// true.
fn run_lua_files_filtered(
    lua: &mlua::Lua,
    dir: &std::path::Path,
    keep: impl Fn(&str) -> bool,
) -> Result<(), String> {
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
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        if !keep(&name) {
            continue;
        }
        let source = match std::fs::read_to_string(&path) {
            Ok(source) => source,
            Err(e) => {
                ctx::runtime_warn(format!(
                    "bone: warning: failed to read {}: {e}",
                    path.display()
                ));
                continue;
            }
        };
        if let Err(e) = lua.load(&source).set_name(&name).exec() {
            ctx::runtime_warn(format!(
                "bone: warning: error executing {}: {e}",
                path.display()
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
#[path = "seed_tests.rs"]
mod seed_tests;

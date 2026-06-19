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

pub use engine::{blank_init_lua, populated_init_lua};
pub use types::{BootOptions, BootResult, BootedTools, EventDispatchResult, ExtensionManager};

include!(concat!(env!("OUT_DIR"), "/default_lua_tools.rs"));
include!(concat!(env!("OUT_DIR"), "/default_lua_commands.rs"));
include!(concat!(env!("OUT_DIR"), "/default_lua_libs.rs"));

use std::collections::HashSet;
use std::path::Path;

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

/// `(filename, description)` for every bundled default tool — drives the setup
/// wizard's tool picker.
pub fn default_tool_catalog() -> Vec<(&'static str, String)> {
    catalog(DEFAULT_LUA_TOOLS)
}

/// `(filename, description)` for every bundled default command.
pub fn default_command_catalog() -> Vec<(&'static str, String)> {
    catalog(DEFAULT_LUA_COMMANDS)
}

fn should_refresh_seeded_lua(path: &Path, name: &str) -> bool {
    let Ok(existing) = std::fs::read_to_string(path) else {
        return false;
    };
    existing.contains("ctx.ui.interact")
        || (name == "ui/menu.lua" && !existing.contains("require(\"ui.pane\")"))
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
    );

    BootedTools {
        manager: extensions,
        tools,
    }
}

/// Seed bundled default Lua tools into the config directory.
/// Existing files are not overwritten except for bundled files that still use
/// the removed Rust interaction API.
/// `allow` filters which bundled tools are seeded: `None` seeds all (default /
/// upgrade behavior), `Some(set)` seeds only the named files. The setup wizard
/// persists the chosen set so both seed paths (startup + Lua boot) agree.
pub fn seed_default_lua_tools(dir: &Path, allow: Option<&HashSet<String>>) {
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("bone: warning: could not create {}: {e}", dir.display());
        return;
    }
    for (name, content) in DEFAULT_LUA_TOOLS {
        if let Some(allow) = allow
            && !allow.contains(*name)
        {
            continue;
        }
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
/// `allow` filters which bundled commands are seeded; see
/// [`seed_default_lua_tools`] for semantics.
pub fn seed_default_lua_commands(dir: &Path, allow: Option<&HashSet<String>>) {
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("bone: warning: could not create {}: {e}", dir.display());
        return;
    }
    for (name, content) in DEFAULT_LUA_COMMANDS {
        if let Some(allow) = allow
            && !allow.contains(*name)
        {
            continue;
        }
        let path = dir.join(name);
        if (!path.exists() || should_refresh_seeded_lua(&path, name))
            && let Err(e) = std::fs::write(&path, content)
        {
            eprintln!("bone: warning: could not write {}: {e}", path.display());
        }
    }
}

#[cfg(test)]
mod seed_tests {
    use super::*;

    #[test]
    fn extract_description_prefers_field_then_comment() {
        assert_eq!(
            extract_description("-- header\nregister_tool({ description = \"does a thing\" })"),
            "does a thing"
        );
        assert_eq!(
            extract_description("-- just a comment\nlocal x = 1"),
            "just a comment"
        );
        assert_eq!(extract_description("local x = 1"), "");
    }

    #[test]
    fn allow_filter_seeds_only_named_files() {
        let dir = std::env::temp_dir().join(format!(
            "bone-seed-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);

        // Pick the first bundled tool to allow, exclude the rest.
        let first = DEFAULT_LUA_TOOLS[0].0.to_string();
        let allow: HashSet<String> = std::iter::once(first.clone()).collect();
        seed_default_lua_tools(&dir, Some(&allow));

        assert!(dir.join(&first).exists(), "allowed file should be seeded");
        for (name, _) in DEFAULT_LUA_TOOLS.iter().skip(1) {
            assert!(
                !dir.join(name).exists(),
                "non-selected file {name} should not be seeded"
            );
        }

        // None seeds everything.
        seed_default_lua_tools(&dir, None);
        for (name, _) in DEFAULT_LUA_TOOLS {
            assert!(dir.join(name).exists(), "{name} should be seeded with None");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Execute all Lua files from a directory.
/// Files are expected to register tools, commands, or other extensions.
pub fn run_lua_files(lua: &mlua::Lua, dir: &std::path::Path) -> Result<(), String> {
    run_lua_files_filtered(lua, dir, |_| true)
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
        let source = std::fs::read_to_string(&path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        lua.load(&source)
            .set_name(&name)
            .exec()
            .map_err(|e| format!("error executing {}: {e}", path.display()))?;
    }

    Ok(())
}

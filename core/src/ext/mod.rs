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
pub mod ops_tools;
pub mod provider_slots;
pub mod settings_registry;
pub mod snapshots;
pub mod source_stamp;
pub mod types;

pub use engine::{blank_init_lua, populated_init_lua};
pub use types::{BootOptions, BootResult, BootedTools, EventDispatchResult, ExtensionManager};

include!(concat!(env!("OUT_DIR"), "/default_lua_plugins.rs"));

/// Name of the always-seeded bundled plugin package. It ships the canonical
/// `/config` command and the shared `banner`/`history`/`ui.*` modules every
/// install relies on, so it can never be deselected.
pub const BUNDLED_CORE_PLUGIN: &str = "core";

/// Legacy flat extensions that cannot become a stem-named package. `/config`
/// now lives in the bundled `core` package, so `commands/config.lua` must never
/// migrate to a `plugins/config` package that would shadow it.
const LEGACY_FLAT_REMAP: &[(&str, &str)] = &[("commands/config.lua", "core/init.lua")];

/// The package name a bundled relative path belongs to (`core/lib/ui/menu.lua`
/// → `core`).
fn bundled_plugin_of(name: &str) -> &str {
    name.split('/').next().unwrap_or_default()
}

/// Names of every bundled default plugin package (top-level directories under
/// `defaults/lua/plugins/`), sorted and deduplicated.
pub fn default_lua_plugin_names() -> Vec<String> {
    let mut names: Vec<String> = DEFAULT_LUA_PLUGINS
        .iter()
        .map(|(name, _)| bundled_plugin_of(name).to_owned())
        .filter(|name| !name.is_empty())
        .collect();
    names.sort();
    names.dedup();
    names
}

use std::collections::HashSet;
use std::path::{Component, Path};

use sha2::{Digest, Sha256};

// These hashes identify pristine bundled config commands. A version marker
// alone cannot distinguish an untouched seed from a user-customized copy, so
// only exact legacy files are safe to replace during migrations.
// SHA-256 899462e75e1316e19d9ec9e6c853672b70b9c88617b3c25c3a743a373152c8fe
const CANONICAL_CONFIG_V6_SHA256: [u8; 32] = [
    137, 148, 98, 231, 94, 19, 22, 225, 157, 158, 201, 230, 200, 83, 103, 43, 112, 185, 200, 134,
    23, 179, 194, 92, 58, 116, 58, 55, 49, 82, 200, 254,
];

// SHA-256 f583a2a1003ca2ab2555ff34a0ce29da0411561dec32e3a1720db34071fe67b4
const CANONICAL_CONFIG_V7_SHA256: [u8; 32] = [
    245, 131, 162, 161, 0, 60, 162, 171, 37, 85, 255, 52, 160, 206, 41, 218, 4, 17, 86, 29, 236,
    50, 227, 161, 114, 13, 179, 64, 113, 254, 103, 180,
];

// SHA-256 702310129a58832935cc155f2c94f75afcb97125fe05d035d2f7c1e9bba546f9
const CANONICAL_CONFIG_V8_SHA256: [u8; 32] = [
    112, 35, 16, 18, 154, 88, 131, 41, 53, 204, 21, 95, 44, 148, 247, 90, 252, 185, 113, 37, 254,
    5, 208, 53, 210, 247, 193, 233, 187, 165, 70, 249,
];

fn is_unmodified_canonical_config(existing: &str) -> bool {
    let digest: [u8; 32] = Sha256::digest(existing.as_bytes()).into();
    (existing.contains("canonical-config-v6") && digest == CANONICAL_CONFIG_V6_SHA256)
        || (existing.contains("canonical-config-v7") && digest == CANONICAL_CONFIG_V7_SHA256)
        || (existing.contains("canonical-config-v8") && digest == CANONICAL_CONFIG_V8_SHA256)
}

fn is_safe_leaf_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains(['/', '\\', '\0'])
        && matches!(
            Path::new(name).components().collect::<Vec<_>>().as_slice(),
            [Component::Normal(_)]
        )
}

/// Whether a path names a Lua source file that Bone discovers and executes.
///
/// Discovery is lowercase-only: an uppercase stem such as `Foo.lua` is
/// ignored, matching the rule documented in `extension-api.md`.
pub(crate) fn is_lowercase_lua_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            Path::new(name).extension().is_some_and(|ext| ext == "lua")
                && name == name.to_lowercase()
        })
}

fn should_refresh_seeded_lua(path: &Path, name: &str) -> std::io::Result<bool> {
    let existing = std::fs::read_to_string(path)?;
    Ok(existing.contains("ctx.ui.interact")
        // Refresh bundled extensions that use the pre-namespace registration API.
        || existing.contains("bone.register_tool")
        || existing.contains("bone.register_command")
        // Refresh menus predating the pane migration or current option-row styling.
        || (name == "core/lib/ui/menu.lua"
            && (!existing.contains("require(\"ui.pane\")")
                || !existing.contains("SELECTED_BG")
                || !existing.contains("description_spans")
                || !existing.contains("label_modifiers")
                || !existing.contains("initial_checked")
                || !existing.contains("preview_row_budget")
                || !existing.contains("multi-space-toggle-v2")))
        // History now includes aggregate message and token counts/status,
        // and lists via a candidate-first CTE instead of a full messages join.
        || (name == "core/lib/history.lua"
            && (!existing.contains("total_token_count") || !existing.contains("WITH recent AS")))
        // Config migrations refresh only exact bundled v6/v7/v8 seeds; preserve
        // any copy the user has edited, even if it has a known marker.
        || (name == "core/init.lua" && is_unmodified_canonical_config(&existing)))
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
    loader::boot(config_dir, cwd, opts, model, provider, None)
}

/// Boot using one canonical settings store shared by all daemon conversation actors.
pub fn boot_shared(
    config_dir: &Path,
    cwd: &Path,
    opts: BootOptions,
    model: &str,
    provider: &str,
    settings: std::sync::Arc<std::sync::Mutex<super::config::settings::Settings>>,
) -> BootResult {
    loader::boot(config_dir, cwd, opts, model, provider, Some(settings))
}

/// Full boot sequence: boot extensions, register Lua tools, and build a
/// configured `ToolHandler` from canonical daemon configuration.
pub fn boot_with_tools(
    config_dir: &Path,
    cwd: &Path,
    config: &super::config::store::ConfigStore,
    _sync: bool,
    opts: BootOptions,
    model: &str,
    provider: &str,
) -> BootedTools {
    boot_with_tools_inner(
        config_dir,
        cwd,
        config,
        opts,
        model,
        provider,
        config.runtime_settings_handle(),
    )
}

#[allow(clippy::too_many_arguments)]
pub fn boot_with_tools_shared(
    config_dir: &Path,
    cwd: &Path,
    config: &super::config::store::ConfigStore,
    _sync: bool,
    opts: BootOptions,
    model: &str,
    provider: &str,
    settings: std::sync::Arc<std::sync::Mutex<super::config::settings::Settings>>,
) -> BootedTools {
    boot_with_tools_inner(config_dir, cwd, config, opts, model, provider, settings)
}

fn configured_tool_names(
    config: &super::config::store::ConfigStore,
    all_tool_names: Vec<String>,
) -> Vec<String> {
    let disabled = config.snapshot().disabled_tools;
    all_tool_names
        .into_iter()
        .filter(|name| !disabled.contains(name))
        .collect()
}

fn boot_with_tools_inner(
    config_dir: &Path,
    cwd: &Path,
    config: &super::config::store::ConfigStore,
    opts: BootOptions,
    model: &str,
    provider: &str,
    settings: std::sync::Arc<std::sync::Mutex<super::config::settings::Settings>>,
) -> BootedTools {
    let tool_allowlist = opts.tool_allowlist.clone();
    let BootResult {
        manager: extensions,
        tools: lua_tools,
        shared_state,
        source_errors,
    } = boot_shared(config_dir, cwd, opts, model, provider, settings);
    if extensions.is_available() {
        config.initialize_extension_catalog(extensions.extension_catalog());
    }

    let mut loaded = super::tools::load_tools();
    super::tools::register_lua_tools(&mut loaded, lua_tools);

    let all_tool_names = loaded
        .registry
        .definitions()
        .iter()
        .map(|definition| definition.name.clone())
        .collect();
    let mut enabled = configured_tool_names(config, all_tool_names);

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
    .with_shared_state(shared_state)
    .with_config_store(config.clone());

    BootedTools {
        manager: extensions,
        tools,
        source_errors,
    }
}

/// Seed bundled default Lua files from `bundled` into `dir`.
///
/// Creates `dir` (and each file's parent) as needed. Only entries for which
/// `keep(name)` is true are considered, where `name` is the bundled path
/// relative to `dir` (e.g. `core/lib/ui/menu.lua`). `force` unconditionally
/// overwrites. Existing files are refreshed when [`should_refresh_seeded_lua`]
/// says so (e.g. they still use a removed Rust interaction API).
fn seed_default_lua(
    dir: &Path,
    bundled: &[(&'static str, &'static str)],
    keep: impl Fn(&str) -> bool,
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
        if !keep(name) {
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

/// Seed bundled default Lua plugin *packages* into `dir` (the `lua/plugins`
/// directory), placing each at `dir/<package>/…`.
///
/// `allow == Some(set)` seeds only packages named in `set`, plus the always-on
/// bundled [`BUNDLED_CORE_PLUGIN`]; `None` seeds every bundled package. See
/// [`seed_default_lua`].
pub fn seed_default_lua_plugins(dir: &Path, allow: Option<&HashSet<String>>, force: bool) {
    seed_default_lua(
        dir,
        DEFAULT_LUA_PLUGINS,
        |name| {
            let package = bundled_plugin_of(name);
            package == BUNDLED_CORE_PLUGIN || allow.is_none_or(|allow| allow.contains(package))
        },
        force,
    )
}

/// One-time, data-preserving migration from the flat extension layout
/// (`lua/{tools,commands}/<stem>.lua`, `lua/lib/<module>.lua`) to plugin
/// packages (`lua/plugins/<name>/`).
///
/// Bundled packages are seeded before this runs, so a destination package may
/// already exist. Every branch preserves the user's bytes somewhere:
/// - a flat file identical to the bundled/current destination is deleted;
/// - a flat file with different bytes is renamed to `<file>.bundled-backup`
///   (never merged, never silently dropped) and a notice is logged;
/// - otherwise the flat file's bytes move into the destination package and the
///   flat file is deleted.
///
/// Empty `tools`/`commands`/`lib` directories left behind are pruned; files are
/// never deleted without a preserved copy.
pub fn migrate_flat_lua_extensions(lua_dir: &Path) {
    if !lua_dir.is_dir() {
        return;
    }

    // Flat tools/commands become packages named after the file stem, except for
    // the documented remaps whose destination is a bundled package.
    for flat_dir in ["tools", "commands"] {
        for flat in sorted_lowercase_lua_files(&lua_dir.join(flat_dir)) {
            let Some(file_name) = flat.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let rel = format!("{flat_dir}/{file_name}");
            if let Some((_, dest_rel)) = LEGACY_FLAT_REMAP.iter().find(|(from, _)| *from == rel) {
                if let Some((_, bundled)) = DEFAULT_LUA_PLUGINS
                    .iter()
                    .find(|(name, _)| name == dest_rel)
                {
                    migrate_remapped_flat_file(&flat, bundled);
                }
                continue;
            }
            let Some(stem) = flat.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            migrate_flat_file(&flat, &lua_dir.join("plugins").join(stem).join("init.lua"));
        }
    }

    // Bundled library modules were previously seeded at `lua/lib/<module>.lua`.
    let core_lib_prefix = format!("{BUNDLED_CORE_PLUGIN}/lib/");
    for (name, bundled) in DEFAULT_LUA_PLUGINS {
        let Some(rel) = name.strip_prefix(core_lib_prefix.as_str()) else {
            continue;
        };
        let flat = lua_dir.join("lib").join(rel);
        match std::fs::read(&flat) {
            Ok(existing) if existing == bundled.as_bytes() => {
                let _ = std::fs::remove_file(&flat);
            }
            Ok(_) => {
                set_aside_bundled_backup(&flat);
            }
            Err(_) => {}
        }
    }

    prune_empty_source_dirs(lua_dir);
}

/// Lowercase `.lua` files directly inside `dir`, sorted. A missing directory
/// yields an empty list.
fn sorted_lowercase_lua_files(dir: &Path) -> Vec<std::path::PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<_> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| is_lowercase_lua_path(path))
        .collect();
    files.sort();
    files
}

/// Relocate a legacy flat extension to a plugin package entry point.
fn migrate_flat_file(flat: &Path, dest: &Path) {
    let Ok(bytes) = std::fs::read(flat) else {
        return;
    };
    match std::fs::read(dest) {
        Ok(existing) if existing == bytes => {
            let _ = std::fs::remove_file(flat);
        }
        Ok(_) => {
            set_aside_bundled_backup(flat);
        }
        Err(_) => {
            if let Some(parent) = dest.parent()
                && let Err(e) = std::fs::create_dir_all(parent)
            {
                ctx::runtime_warn(format!(
                    "bone: warning: could not create {}: {e}",
                    parent.display()
                ));
                return;
            }
            let permissions = std::fs::metadata(flat).ok().map(|meta| meta.permissions());
            if let Err(e) = crate::tools::write_atomic::write_atomic_sync(dest, &bytes, permissions)
            {
                ctx::runtime_warn(format!(
                    "bone: warning: could not write {}: {e}",
                    dest.display()
                ));
                return;
            }
            let _ = std::fs::remove_file(flat);
        }
    }
}

/// Handle a legacy flat file whose destination is a bundled package entry
/// point. Identical bytes are redundant; anything else is the user's own copy,
/// set aside so it can never shadow the bundled package.
fn migrate_remapped_flat_file(flat: &Path, bundled: &str) {
    match std::fs::read(flat) {
        Ok(existing) if existing == bundled.as_bytes() => {
            let _ = std::fs::remove_file(flat);
        }
        Ok(_) => {
            set_aside_bundled_backup(flat);
        }
        Err(_) => {}
    }
}

/// Rename a legacy flat file to `<file>.bundled-backup` so it can never shadow
/// the plugin package that now owns its name. The copy is preserved, never
/// merged; a notice is logged so the move is discoverable.
fn set_aside_bundled_backup(flat: &Path) {
    let mut backup = flat.as_os_str().to_os_string();
    backup.push(".bundled-backup");
    let backup = std::path::PathBuf::from(backup);
    match std::fs::rename(flat, &backup) {
        Ok(()) => ctx::runtime_warn(format!(
            "bone: {} was set aside as {}; the current copy lives under lua/plugins/",
            flat.display(),
            backup.display()
        )),
        Err(e) => ctx::runtime_warn(format!(
            "bone: warning: could not set aside {}: {e}",
            flat.display()
        )),
    }
}

/// Remove the now-empty flat extension directories (`tools`, `commands`, `lib`),
/// deepest first. Files that remain (e.g. user-authored additions or backups)
/// keep their directories in place.
fn prune_empty_source_dirs(lua_dir: &Path) {
    for flat_dir in ["tools", "commands", "lib"] {
        let root = lua_dir.join(flat_dir);
        let mut stack = vec![root.clone()];
        let mut dirs = Vec::new();
        while let Some(dir) = stack.pop() {
            dirs.push(dir.clone());
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        stack.push(path);
                    }
                }
            }
        }
        dirs.sort_by_key(|dir| std::cmp::Reverse(dir.components().count()));
        for dir in dirs {
            let _ = std::fs::remove_dir(&dir);
        }
    }
}

/// Execute the `init.lua` entry point of every plugin package under
/// `plugins_dir` (one directory level, sorted by name).
///
/// A plugin is a *package*: `lua/plugins/<name>/init.lua` runs at boot and
/// registers ordinary capabilities (`bone.tool.register`,
/// `bone.command.register`, `bone.on`, …). While a plugin's entry point runs,
/// `bone._plugin_owner` is set to the plugin name so each registration records
/// which plugin it came from; capabilities therefore inherit their plugin's
/// enable state.
///
/// Each enabled package's directory is added to `package.path` (see
/// [`extend_plugin_package_path`]) so plugin code can `require` its own
/// submodules — `require("lib.util")` resolves under the package's `lib/`
/// before the global `lua/lib` is consulted.
///
/// A directory without `init.lua` is an inert, non-error package. Names in
/// `disabled` are skipped without being removed from disk; `disabled == None`
/// runs every installed plugin.
pub fn run_lua_plugin_files(
    lua: &mlua::Lua,
    plugins_dir: &std::path::Path,
    disabled: Option<&HashSet<String>>,
) -> Result<(), String> {
    if !plugins_dir.is_dir() {
        return Ok(());
    }

    let mut dirs: Vec<_> = std::fs::read_dir(plugins_dir)
        .map_err(|e| format!("failed to read {}: {e}", plugins_dir.display()))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();

    let mut errors = Vec::new();
    for dir in dirs {
        let name = dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        if disabled.is_some_and(|disabled| disabled.contains(&name)) {
            continue;
        }
        let init = dir.join("init.lua");
        if !init.is_file() {
            continue;
        }
        extend_plugin_package_path(lua, &dir);
        if let Err(e) = exec_lua_file(lua, &init, &name, Some(&name)) {
            ctx::runtime_warn(format!("bone: warning: plugin '{name}': {e}"));
            errors.push(format!("plugin '{name}': {e}"));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

/// Append a plugin package's module patterns to `package.path` so its code
/// can `require` its own files: `require("x")` resolves against
/// `<pkg>/x.lua`, `<pkg>/lib/x.lua`, and `<pkg>/lib/x/init.lua`.
///
/// The patterns are appended *after* the existing ones, so the global
/// `lua/lib` wins any name collision (a plugin's own `ui.menu` can never
/// shadow the seeded one). Idempotent: patterns already present are not
/// re-appended, which keeps hot reloads from duplicating search paths.
fn extend_plugin_package_path(lua: &mlua::Lua, package_dir: &std::path::Path) {
    let Ok(package) = lua.globals().get::<mlua::Table>("package") else {
        return;
    };
    let Ok(existing) = package.get::<String>("path") else {
        return;
    };
    let dir = package_dir.to_string_lossy();
    let patterns = [
        format!("{dir}/?.lua"),
        format!("{dir}/lib/?.lua"),
        format!("{dir}/lib/?/init.lua"),
    ];
    let mut updated = existing.clone();
    for pattern in &patterns {
        if !updated.contains(pattern.as_str()) {
            if !updated.ends_with(';') {
                updated.push(';');
            }
            updated.push_str(pattern);
        }
    }
    if updated != existing {
        let _ = package.set("path", updated);
    }
}

/// Names of the installed plugin packages under `plugins_dir` (one directory
/// level, sorted): each directory that contains an `init.lua`. Directories
/// without one are inert and omitted. Disabled plugins are still listed so
/// their enable/disable toggle remains reachable.
pub fn installed_plugin_names(plugins_dir: &std::path::Path) -> Vec<String> {
    if !plugins_dir.is_dir() {
        return Vec::new();
    }
    let mut names: Vec<_> = std::fs::read_dir(plugins_dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir() && path.join("init.lua").is_file())
        .map(|path| {
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

/// Execute a single Lua file `path` (Lua chunk name `name`). While the file
/// runs, `bone._settings_owner` is set to the file path so settings writes can
/// be rolled back on error via `bone.settings._rollback_owner`. When
/// `plugin_owner` is `Some(name)`, `bone._plugin_owner` is additionally set for
/// the duration so registrations record which plugin they came from. Both
/// globals are reset afterwards.
fn exec_lua_file(
    lua: &mlua::Lua,
    path: &std::path::Path,
    name: &str,
    plugin_owner: Option<&str>,
) -> Result<(), String> {
    let source = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    let owner = path.to_string_lossy().to_string();
    let bone = lua.globals().get::<mlua::Table>("bone").ok();
    if let Some(bone) = &bone {
        bone.set("_settings_owner", owner.as_str())
            .map_err(crate::util::errstr)?;
        if let Some(plugin) = plugin_owner {
            bone.set("_plugin_owner", plugin)
                .map_err(crate::util::errstr)?;
        }
    }
    let result = lua.load(&source).set_name(name).exec();
    if result.is_err()
        && let Some(bone) = &bone
        && let Ok(settings) = bone.get::<mlua::Table>("settings")
        && let Ok(rollback) = settings.get::<mlua::Function>("_rollback_owner")
    {
        let _ = rollback.call::<()>(owner);
    }
    if let Some(bone) = bone {
        bone.set("_settings_owner", mlua::Value::Nil)
            .map_err(crate::util::errstr)?;
        if plugin_owner.is_some() {
            bone.set("_plugin_owner", mlua::Value::Nil)
                .map_err(crate::util::errstr)?;
        }
    }
    result.map_err(|e| format!("error executing {}: {e}", path.display()))
}

#[cfg(test)]
#[path = "seed_tests.rs"]
mod seed_tests;

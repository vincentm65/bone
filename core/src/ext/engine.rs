//! Lua engine — creates the `mlua::Lua` state, populates the `bone` global
//! table, and executes `init.lua`.

use std::path::Path;

use mlua::{Function, Lua, LuaSerdeExt, Result as LuaResult, Table};

use super::types::BootOptions;

/// Default `init.lua`: lightweight wiring only. Substantial banner logic lives
/// in Bone-owned core's `lib/banner.lua` module.
const DEFAULT_INIT_LUA: &str = r#"-- Bone init.lua
require("banner")
"#;

/// Minimal `init.lua` written when the user opts out of auto-population.
pub const BLANK_INIT_LUA: &str = "-- Bone init.lua
-- Empty by choice. Define bone.banner or add advanced startup hooks here.
-- See the docs for the full picture of what Lua can do.
";

/// Banner wiring — the \"auto-populated\" onboarding choice. Named sub-agents
/// are persisted separately in `subagents.yaml`.
pub fn populated_init_lua() -> String {
    DEFAULT_INIT_LUA.to_string()
}

/// Minimal `init.lua` — the \"blank\" onboarding choice.
pub fn blank_init_lua() -> String {
    BLANK_INIT_LUA.to_string()
}

/// Build a ready-to-use Lua state with the `bone` table populated.
#[allow(clippy::too_many_arguments)]
pub(crate) fn create_engine(
    version: &str,
    cwd: &Path,
    config_dir: &Path,
    opts: BootOptions,
    model: &str,
    provider: &str,
    shared_ui: super::api_ui::SharedUi,
    settings: std::sync::Arc<std::sync::Mutex<crate::config::settings::Settings>>,
    settings_registry: super::settings_registry::SharedSettingsRegistry,
) -> Result<Lua, String> {
    let lua = Lua::new();

    let globals = lua.globals();

    // Sandbox dangerous globals.
    sandbox_globals(&lua, &globals)?;

    // Create the `bone` table.
    let bone = lua.create_table().map_err(crate::util::errstr)?;

    bone.set("version", version).map_err(crate::util::errstr)?;

    bone.set("cwd", cwd.to_string_lossy().to_string())
        .map_err(crate::util::errstr)?;

    bone.set("config_dir", config_dir.to_string_lossy().to_string())
        .map_err(crate::util::errstr)?;

    // Conventional location for user-supplied native helper binaries. Bone no
    // longer pre-creates this directory; the user creates it when needed. It is
    // exposed so scripts locate it without hard-coding paths. Bone never
    // discovers or executes files here — helpers are invoked explicitly through
    // approved shell or process APIs, and their contents are outside the Lua
    // source fingerprint.
    bone.set(
        "helpers_dir",
        config_dir.join("lua/helpers").to_string_lossy().to_string(),
    )
    .map_err(crate::util::errstr)?;

    // Boot context: scripts can adapt to nesting depth and headless mode
    // (e.g. the subagent tool refuses to register inside sub-agent VMs).
    bone.set("agent_depth", opts.agent_depth)
        .map_err(crate::util::errstr)?;
    bone.set("headless", opts.headless)
        .map_err(crate::util::errstr)?;

    // Shared truncation marker exposed to Lua so the subagent tool and the
    // Rust inline-injection path stay in sync (see jobs::TRUNCATION_MARKER).
    bone.set("truncation_marker", crate::ext::jobs::TRUNCATION_MARKER)
        .map_err(crate::util::errstr)?;

    // Model and provider — set before init.lua runs so banner() can read them.
    bone.set("model", model).map_err(crate::util::errstr)?;
    bone.set("provider", provider)
        .map_err(crate::util::errstr)?;

    // bone.log table (writes to a log file to avoid corrupting the TUI)
    let log = create_log_table(&lua, config_dir).map_err(crate::util::errstr)?;
    bone.set("log", log).map_err(crate::util::errstr)?;
    // Override the global `print` to route through `lua_log` (bone.log +
    // headless stderr) instead of stdout. The TUI owns stdout in raw mode, so a
    // stray `print()` in a tool/command would otherwise scramble the screen.
    let print_config_dir = config_dir.to_string_lossy().to_string();
    let print_fn = lua
        .create_function(move |lua, args: mlua::Variadic<mlua::Value>| {
            // Mirror real `print`: stringify every argument via Lua's `tostring`
            // (honoring `__tostring`) so non-string args — nil, booleans, tables
            // — log instead of raising, then join with tabs.
            let tostring: mlua::Function = lua.globals().get("tostring")?;
            let parts: Vec<String> = args
                .into_iter()
                .map(|v| tostring.call::<String>(v))
                .collect::<mlua::Result<_>>()?;
            super::ctx::lua_log(&print_config_dir, "info", &parts.join("\t"));
            Ok(())
        })
        .map_err(crate::util::errstr)?;
    globals
        .set("print", print_fn)
        .map_err(crate::util::errstr)?;

    // `require` resolves against the bundled `core` package's `lib/` first, then
    // the user's `lua/lib` module root. Core comes first so fresh bundled modules
    // (`require("ui.menu")`, `require("banner")`, `require("history")`) always
    // win over stale copies left in `lua/lib`; `lua/lib` remains a searchable root
    // for user modules whose names do not collide.
    let core_lib_dir = config_dir.join("lua/core/lib");
    let lua_lib_dir = config_dir.join("lua").join("lib");
    let package: Table = globals
        .get("package")
        .map_err(|e| format!("failed to get package table: {e}"))?;
    let existing_path: String = package.get("path").unwrap_or_else(|_| ";".to_string());
    let sep = if existing_path.ends_with(';') {
        ""
    } else {
        ";"
    };
    let core_lib_dir = core_lib_dir.to_string_lossy();
    let lua_lib_dir = lua_lib_dir.to_string_lossy();
    let new_path = format!(
        "{core}/?.lua;{core}/?/init.lua;{lib}/?.lua;{lib}/?/init.lua{sep}{existing_path}",
        core = core_lib_dir,
        lib = lua_lib_dir,
    );
    package.set("path", new_path).map_err(crate::util::errstr)?;

    globals.set("bone", bone).map_err(crate::util::errstr)?;

    // Inject cjson global (encode/decode via serde_json).
    inject_cjson(&lua, &globals)?;

    // bone.tool.register + bone._tools array
    let bone = &globals.get::<Table>("bone").map_err(crate::util::errstr)?;
    super::ops_tools::setup_register_tool(&lua, bone)?;
    super::ops_tools::setup_register_subagent(&lua, bone, settings.clone())?;
    super::ops_commands::setup_register_command(&lua, bone)?;
    super::ops_events::setup_on(&lua, bone)?;
    // bone.api.ui.* — the minimal Lua UI API (Phase 4). Additive namespace,
    // backed by a per-VM ViewModel in Lua app-data.
    super::api_ui::setup_api_ui(&lua, bone, shared_ui.clone())?;
    // bone.api.{autocmd,emit,keymap,config} — the always-available runtime API
    // (Phase 6). Must run after `setup_on` so `bone.api.autocmd` can alias it.
    super::api::setup_api(
        &lua,
        bone,
        settings,
        settings_registry,
        config_dir.join("config.yaml"),
        shared_ui,
    )?;

    Ok(lua)
}

/// Load and execute the startup `init.lua`. Returns `Ok(true)` if at least one
/// init file existed and ran without errors, `Ok(false)` if none existed.
///
/// Two locations are accepted and run in order, in the same VM:
/// 1. `config_dir/init.lua` (canonical root)
/// 2. `config_dir/lua/init.lua` (module-directory form)
///
/// When neither exists, a blank root `init.lua` is created automatically so the
/// user has an obvious place to start wiring. A failing file is rolled back on
/// its own and reported; errors are aggregated so boot can surface them.
pub(crate) fn run_init(lua: &Lua, config_dir: &Path) -> Result<bool, String> {
    let root_init = config_dir.join("init.lua");
    let nested_init = config_dir.join("lua").join("init.lua");

    let root_exists = root_init.exists();
    let nested_exists = nested_init.exists();

    if !root_exists && !nested_exists {
        std::fs::write(&root_init, DEFAULT_INIT_LUA)
            .map_err(|e| format!("failed to create init.lua: {e}"))?;
        return Ok(false);
    }

    let mut ran = false;
    let mut errors = Vec::new();
    if root_exists {
        match exec_init_file(lua, &root_init, config_dir, "init.lua") {
            Ok(()) => ran = true,
            Err(e) => errors.push(e),
        }
    }
    if nested_exists {
        match exec_init_file(lua, &nested_init, config_dir, "lua/init.lua") {
            Ok(()) => ran = true,
            Err(e) => errors.push(e),
        }
    }

    if errors.is_empty() {
        Ok(ran)
    } else {
        Err(errors.join("; "))
    }
}

/// Read and execute one startup init file, rolling back settings owned by
/// `"init.lua"` on failure. `name` is both the Lua chunk name and error prefix.
fn exec_init_file(lua: &Lua, path: &Path, config_dir: &Path, name: &str) -> Result<(), String> {
    let source =
        std::fs::read_to_string(path).map_err(|e| format!("failed to read {name}: {e}"))?;

    match lua.load(&source).set_name(name).exec() {
        Ok(()) => Ok(()),
        Err(e) => {
            if let Ok(bone) = lua.globals().get::<Table>("bone")
                && let Ok(settings) = bone.get::<Table>("settings")
                && let Ok(rollback) = settings.get::<Function>("_rollback_owner")
            {
                let _ = rollback.call::<()>("init.lua");
            }
            let message = format!("{name} error: {e}");
            super::ctx::lua_log(&config_dir.to_string_lossy(), "warn", &message);
            Err(message)
        }
    }
}

/// Create the `bone.log` sub-table with `info`, `warn`, `error` functions.
fn create_log_table(lua: &Lua, config_dir: &Path) -> LuaResult<Table> {
    let log = lua.create_table()?;
    let log_path = config_dir.join("bone.log");

    let make_log_fn = |lua: &Lua, level: &str| -> LuaResult<Function> {
        let log_path = log_path.clone();
        let level = level.to_string();
        lua.create_function(move |_, msg: String| {
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let line = format!("[{ts}] bone-lua {level}: {msg}\n");
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
            {
                use std::io::Write;
                let _ = write!(f, "{line}");
            }
            Ok(())
        })
    };

    log.set("info", make_log_fn(lua, "info")?)?;
    log.set("warn", make_log_fn(lua, "warn")?)?;
    log.set("error", make_log_fn(lua, "error")?)?;

    Ok(log)
}

/// Replace dangerous `os` and `io` entries with error stubs.
fn sandbox_globals(lua: &Lua, globals: &Table) -> Result<(), String> {
    if let Ok(Some(os)) = globals.get::<Option<Table>>("os") {
        sandbox_table(
            lua,
            &os,
            &["execute", "exit", "remove", "rename", "tmpname"],
        )?;
    }

    if let Ok(Some(io)) = globals.get::<Option<Table>>("io") {
        sandbox_table(
            lua,
            &io,
            &[
                "open", "popen", "tmpfile", "input", "lines", "output", "read", "write", "flush",
                "close",
            ],
        )?;
    }

    // Block package.loadlib to prevent loading native C modules.
    if let Ok(Some(package)) = globals.get::<Option<Table>>("package") {
        let loadlib_stub = lua
            .create_function(|_, _: mlua::Value| -> LuaResult<()> {
                Err(mlua::Error::external(
                    "not available in bone Lua sandbox; use ctx APIs instead",
                ))
            })
            .map_err(crate::util::errstr)?;
        package
            .set("loadlib", loadlib_stub)
            .map_err(crate::util::errstr)?;
    }

    let stub = lua
        .create_function(|_, _: mlua::Value| -> LuaResult<()> {
            Err(mlua::Error::external(
                "not available in bone Lua sandbox; use ctx APIs instead",
            ))
        })
        .map_err(crate::util::errstr)?;
    globals
        .set("dofile", stub.clone())
        .map_err(crate::util::errstr)?;
    globals.set("loadfile", stub).map_err(crate::util::errstr)?;

    Ok(())
}

fn sandbox_table(lua: &Lua, table: &Table, keys: &[&str]) -> Result<(), String> {
    let stub = lua
        .create_function(|_, _: mlua::Value| -> LuaResult<()> {
            Err(mlua::Error::external(
                "not available in bone Lua sandbox; use ctx APIs instead",
            ))
        })
        .map_err(crate::util::errstr)?;
    for &key in keys {
        table.set(key, stub.clone()).map_err(crate::util::errstr)?;
    }
    Ok(())
}

/// Inject a `cjson` global table with `encode` and `decode` functions
/// backed by serde_json. This matches the lua-cjson API used by seeded tools.
fn inject_cjson(lua: &Lua, globals: &Table) -> Result<(), String> {
    let cjson = lua.create_table().map_err(crate::util::errstr)?;

    let encode_fn = lua
        .create_function(|lua, value: mlua::Value| {
            let json: serde_json::Value = lua.from_value(value)?;
            let s = serde_json::to_string(&json)
                .map_err(|e| mlua::Error::external(format!("cjson.encode: {e}")))?;
            Ok(s)
        })
        .map_err(crate::util::errstr)?;
    cjson
        .set("encode", encode_fn)
        .map_err(crate::util::errstr)?;

    let decode_fn = lua
        .create_function(|lua, s: String| {
            let json: serde_json::Value = serde_json::from_str(&s)
                .map_err(|e| mlua::Error::external(format!("cjson.decode: {e}")))?;
            let value = lua.to_value(&json)?;
            Ok(value)
        })
        .map_err(crate::util::errstr)?;
    cjson
        .set("decode", decode_fn)
        .map_err(crate::util::errstr)?;

    globals.set("cjson", cjson).map_err(crate::util::errstr)?;
    Ok(())
}

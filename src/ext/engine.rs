//! Lua engine — creates the `mlua::Lua` state, populates the `bone` global
//! table, and executes `init.lua`.

use std::path::Path;

use mlua::{Lua, LuaSerdeExt, Result as LuaResult, Table};

use super::types::BootOptions;

/// Build a ready-to-use Lua state with the `bone` table populated.
pub(crate) fn create_engine(
    version: &str,
    cwd: &Path,
    config_dir: &Path,
    opts: BootOptions,
) -> Result<Lua, String> {
    let lua = Lua::new();

    let globals = lua.globals();

    // Sandbox dangerous globals.
    sandbox_globals(&lua, &globals)?;

    // Create the `bone` table.
    let bone = lua.create_table().map_err(|e| e.to_string())?;

    bone.set("version", version).map_err(|e| e.to_string())?;

    bone.set("cwd", cwd.to_string_lossy().to_string())
        .map_err(|e| e.to_string())?;

    bone.set("config_dir", config_dir.to_string_lossy().to_string())
        .map_err(|e| e.to_string())?;

    // Boot context: scripts can adapt to nesting depth and headless mode
    // (e.g. the subagent tool refuses to register inside sub-agent VMs).
    bone.set("agent_depth", opts.agent_depth)
        .map_err(|e| e.to_string())?;
    bone.set("headless", opts.headless)
        .map_err(|e| e.to_string())?;

    // bone.log table
    let log = create_log_table(&lua).map_err(|e| e.to_string())?;
    bone.set("log", log).map_err(|e| e.to_string())?;

    // Set safe package.path entries so users can `require` from their lua dir.
    let lua_dir = config_dir.join("lua");
    let package: Table = globals
        .get("package")
        .map_err(|e| format!("failed to get package table: {e}"))?;
    let existing_path: String = package.get("path").unwrap_or_else(|_| ";".to_string());
    let sep = if existing_path.ends_with(';') {
        ""
    } else {
        ";"
    };
    let lua_dir_str = lua_dir.to_string_lossy();
    let new_path = format!(
        "{lua_dir_str}/?.lua;{lua_dir_str}/?/init.lua{sep}{existing_path}",
        lua_dir_str = lua_dir_str,
    );
    package.set("path", new_path).map_err(|e| e.to_string())?;

    globals.set("bone", bone).map_err(|e| e.to_string())?;

    // Inject cjson global (encode/decode via serde_json).
    inject_cjson(&lua, &globals)?;

    // bone.register_tool + bone._tools array
    let bone = &globals.get::<Table>("bone").map_err(|e| e.to_string())?;
    super::ops_tools::setup_register_tool(&lua, bone)?;
    super::ops_tools::setup_register_subagent(&lua, bone)?;
    super::ops_commands::setup_register_command(&lua, bone)?;
    super::ops_events::setup_on(&lua, bone)?;
    super::ops_plugins::setup_plugin(&lua, bone)?;

    Ok(lua)
}

/// Load and execute `init.lua`. Returns `Ok(true)` if the file existed and
/// ran without errors. Returns `Ok(false)` if the file is missing.
/// If `init.lua` does not exist, a blank one is created automatically.
pub(crate) fn run_init(lua: &Lua, config_dir: &Path) -> Result<bool, String> {
    let init_path = config_dir.join("init.lua");
    if !init_path.exists() {
        std::fs::write(&init_path, "")
            .map_err(|e| format!("failed to create init.lua: {e}"))?;
        return Ok(false);
    }

    let source =
        std::fs::read_to_string(&init_path).map_err(|e| format!("failed to read init.lua: {e}"))?;

    match lua.load(&source).set_name("init.lua").exec() {
        Ok(()) => Ok(true),
        Err(e) => {
            eprintln!("bone: warning: init.lua error: {e}");
            Ok(false)
        }
    }
}

/// Create the `bone.log` sub-table with `info`, `warn`, `error` functions.
fn create_log_table(lua: &Lua) -> LuaResult<Table> {
    let log = lua.create_table()?;

    let info_fn = lua.create_function(|_, msg: String| {
        eprintln!("bone-lua: {msg}");
        Ok(())
    })?;
    log.set("info", info_fn)?;

    let warn_fn = lua.create_function(|_, msg: String| {
        eprintln!("bone-lua warn: {msg}");
        Ok(())
    })?;
    log.set("warn", warn_fn)?;

    let error_fn = lua.create_function(|_, msg: String| {
        eprintln!("bone-lua error: {msg}");
        Ok(())
    })?;
    log.set("error", error_fn)?;

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
            .map_err(|e| e.to_string())?;
        package
            .set("loadlib", loadlib_stub)
            .map_err(|e| e.to_string())?;
    }

    let stub = lua
        .create_function(|_, _: mlua::Value| -> LuaResult<()> {
            Err(mlua::Error::external(
                "not available in bone Lua sandbox; use ctx APIs instead",
            ))
        })
        .map_err(|e| e.to_string())?;
    globals
        .set("dofile", stub.clone())
        .map_err(|e| e.to_string())?;
    globals.set("loadfile", stub).map_err(|e| e.to_string())?;

    Ok(())
}

fn sandbox_table(lua: &Lua, table: &Table, keys: &[&str]) -> Result<(), String> {
    let stub = lua
        .create_function(|_, _: mlua::Value| -> LuaResult<()> {
            Err(mlua::Error::external(
                "not available in bone Lua sandbox; use ctx APIs instead",
            ))
        })
        .map_err(|e| e.to_string())?;
    for &key in keys {
        table.set(key, stub.clone()).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Inject a `cjson` global table with `encode` and `decode` functions
/// backed by serde_json. This matches the lua-cjson API used by seeded tools.
fn inject_cjson(lua: &Lua, globals: &Table) -> Result<(), String> {
    let cjson = lua.create_table().map_err(|e| e.to_string())?;

    let encode_fn = lua
        .create_function(|lua, value: mlua::Value| {
            let json: serde_json::Value = lua.from_value(value)?;
            let s = serde_json::to_string(&json)
                .map_err(|e| mlua::Error::external(format!("cjson.encode: {e}")))?;
            Ok(s)
        })
        .map_err(|e| e.to_string())?;
    cjson.set("encode", encode_fn).map_err(|e| e.to_string())?;

    let decode_fn = lua
        .create_function(|lua, s: String| {
            let json: serde_json::Value = serde_json::from_str(&s)
                .map_err(|e| mlua::Error::external(format!("cjson.decode: {e}")))?;
            let value = lua.to_value(&json)?;
            Ok(value)
        })
        .map_err(|e| e.to_string())?;
    cjson.set("decode", decode_fn).map_err(|e| e.to_string())?;

    globals.set("cjson", cjson).map_err(|e| e.to_string())?;
    Ok(())
}

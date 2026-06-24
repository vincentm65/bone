//! Lua engine — creates the `mlua::Lua` state, populates the `bone` global
//! table, and executes `init.lua`.

use std::path::Path;

use mlua::{Function, Lua, LuaSerdeExt, Result as LuaResult, Table};

use super::types::BootOptions;

/// Default `init.lua` content — defines `bone.banner` for the startup/clear banner.
const DEFAULT_INIT_LUA: &str = r#"-- Bone init.lua
-- Customize or replace this file to change the banner and other Lua hooks.

-- Default banner function. Override to change the startup/clear banner.
-- Available globals:
--   bone.version     — e.g. "2.1.0"
--   bone.cwd         — current working directory
--   bone.model       — model name (e.g. "gpt-4o")
--   bone.provider    — provider name (e.g. "OpenAI (openai)")
--   bone.term_width  — terminal width in columns (e.g. 120)
--   bone.headless    — true if running without TUI
--   bone.config_dir  — path to ~/.bone-rust
--
-- Return a table of strings. Each string is one banner line, rendered as
-- plain text (no per-substring color). Rust only bolds a line that equals
-- exactly "bone". The full-width box is drawn here so it adapts to term_width.
local function width(s)           -- display width (codepoint count)
    local n = 0
    for _ in utf8.codes(s) do n = n + 1 end
    return n
end

local function short_dir(path)    -- path -> first/.../last
    local parts = {}
    for seg in path:gmatch("[^/]+") do parts[#parts + 1] = seg end
    if #parts > 2 then
        local first = (path:sub(1, 1) == "/") and "/" or parts[1]
        local last = parts[#parts]
        local sep = (first:sub(-1) == "/") and "" or "/"
        return first .. sep .. ".../" .. last
    end
    return path
end

bone.banner = function()
    local w = bone.api.ui.term_width()
    local content_w = w - 3

    -- Padded row: │ left  <pad>  right │, filling w columns.
    local function row(left, right)
        local pad = math.max(0, content_w - width(left) - width(right))
        local spaces = math.max(0, pad - 1)
        return "│ " .. left .. (" "):rep(spaces) .. right .. " │"
    end

    local rule = ("─"):rep(math.max(0, w - 2))

    return {
        "╭" .. rule .. "╮",
        row("bone", "v" .. bone.version),
        row(bone.provider .. " · " .. bone.model, short_dir(bone.cwd)),
        "╰" .. rule .. "╯",
    }
end
"#;

/// Appended to the banner template when the user opts into a populated
/// `init.lua` during onboarding: a live, ready-to-dispatch sub-agent.
const SUBAGENT_INIT_SNIPPET: &str = r#"
-- A ready-to-use sub-agent, live immediately. Dispatch it with the `subagent`
-- tool, e.g. "use the researcher subagent to investigate X". Add more with
-- additional bone.register_subagent { ... } calls, or delete this block.
bone.register_subagent({
    name = "researcher",
    description = "Investigates a question across the codebase and reports concise findings.",
    system_prompt = "You are a focused research agent. Investigate the assigned task "
        .. "thoroughly using the available tools, then report concrete findings with "
        .. "file:line references. Do not make edits.",
})
"#;

/// Minimal `init.lua` written when the user opts out of auto-population.
pub const BLANK_INIT_LUA: &str = "-- Bone init.lua
-- Empty by choice. Define bone.banner, register sub-agents, or add event hooks
-- here. See the docs for the full picture of what Lua can do.
";

/// Banner template + a live sub-agent — the \"auto-populated\" onboarding choice.
pub fn populated_init_lua() -> String {
    format!("{DEFAULT_INIT_LUA}{SUBAGENT_INIT_SNIPPET}")
}

/// Minimal `init.lua` — the \"blank\" onboarding choice.
pub fn blank_init_lua() -> String {
    BLANK_INIT_LUA.to_string()
}

/// Build a ready-to-use Lua state with the `bone` table populated.
pub(crate) fn create_engine(
    version: &str,
    cwd: &Path,
    config_dir: &Path,
    opts: BootOptions,
    model: &str,
    provider: &str,
    shared_ui: super::api_ui::SharedUi,
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
    bone.set("provider", provider).map_err(crate::util::errstr)?;

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
    globals.set("print", print_fn).map_err(crate::util::errstr)?;

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
    let lua_lib_dir = lua_dir.join("lib");
    let lua_lib_dir_str = lua_lib_dir.to_string_lossy();
    let new_path = format!(
        "{lua_dir_str}/?.lua;{lua_dir_str}/?/init.lua;{lua_lib_dir_str}/?.lua;{lua_lib_dir_str}/?/init.lua{sep}{existing_path}",
        lua_dir_str = lua_dir_str,
        lua_lib_dir_str = lua_lib_dir_str,
    );
    package.set("path", new_path).map_err(crate::util::errstr)?;

    globals.set("bone", bone).map_err(crate::util::errstr)?;

    // Inject cjson global (encode/decode via serde_json).
    inject_cjson(&lua, &globals)?;

    // bone.register_tool + bone._tools array
    let bone = &globals.get::<Table>("bone").map_err(crate::util::errstr)?;
    super::ops_tools::setup_register_tool(&lua, bone)?;
    super::ops_tools::setup_register_subagent(&lua, bone)?;
    super::ops_commands::setup_register_command(&lua, bone)?;
    super::ops_events::setup_on(&lua, bone)?;
    super::ops_plugins::setup_plugin(&lua, bone)?;
    // bone.api.ui.* — the minimal Lua UI API (Phase 4). Additive namespace,
    // backed by a per-VM ViewModel in Lua app-data.
    super::api_ui::setup_api_ui(&lua, bone, shared_ui)?;
    // bone.api.{autocmd,emit,keymap,config} — the always-available runtime API
    // (Phase 6). Must run after `setup_on` so `bone.api.autocmd` can alias it.
    super::api::setup_api(&lua, bone)?;

    Ok(lua)
}

/// Load and execute `init.lua`. Returns `Ok(true)` if the file existed and
/// ran without errors. Returns `Ok(false)` if the file is missing.
/// If `init.lua` does not exist, a blank one is created automatically.
pub(crate) fn run_init(lua: &Lua, config_dir: &Path) -> Result<bool, String> {
    let init_path = config_dir.join("init.lua");
    if !init_path.exists() {
        let default_init = DEFAULT_INIT_LUA;
        std::fs::write(&init_path, default_init)
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
    cjson.set("encode", encode_fn).map_err(crate::util::errstr)?;

    let decode_fn = lua
        .create_function(|lua, s: String| {
            let json: serde_json::Value = serde_json::from_str(&s)
                .map_err(|e| mlua::Error::external(format!("cjson.decode: {e}")))?;
            let value = lua.to_value(&json)?;
            Ok(value)
        })
        .map_err(crate::util::errstr)?;
    cjson.set("decode", decode_fn).map_err(crate::util::errstr)?;

    globals.set("cjson", cjson).map_err(crate::util::errstr)?;
    Ok(())
}

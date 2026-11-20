//! Shared Lua setup for both Lua states (core and TUI).
//!
//! - The `runtime/` directory is embedded in the binary. A file with the same
//!   relative path under `<config dir>/runtime/` replaces the embedded one, so
//!   any default can be swapped out.
//! - `require("x.y")` searches `<config dir>/lua/x/y.lua` (and `x/y/init.lua`),
//!   then the runtime's `lua/` directory.
//! - The global `bone` table starts with `bone.side`, `bone.version`,
//!   `bone.config_dir`, `bone.json`, `bone.util` and `bone.inspect`; each side
//!   adds its own API on top.

pub mod sys;
pub mod wait;

use std::path::{Path, PathBuf};

use mlua::{ExternalResult, Function, Lua, LuaSerdeExt, MultiValue, Table, Value};

// `RUNTIME`: every file in the repository's `runtime/` directory, by
// relative path (written by build.rs).
include!(concat!(env!("OUT_DIR"), "/runtime_files.rs"));

/// The customization guides and the references they point to, embedded.
/// Exposed to both sides as `bone.docs`, and written to `<config dir>/docs/`
/// at startup by [`write_docs`].
macro_rules! docs_files {
    ($($path:literal),* $(,)?) => {
        &[$(($path, include_str!(concat!("../../../docs/", $path)))),*]
    };
}

pub const DOCS: &[(&str, &str)] = docs_files![
    "customizing.md",
    "customizing/customizing-tui.md",
    "customizing/customizing-gui.md",
    "customizing/customizing-panels.md",
    "customizing/customizing-popups.md",
    "customizing/customizing-chat.md",
    "customizing/customizing-core.md",
    "customizing/customizing-plugins.md",
    "lua.md",
    "architecture.md",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Core,
    Tui,
    Gui,
}

impl Side {
    pub fn name(self) -> &'static str {
        match self {
            Side::Core => "core",
            Side::Tui => "tui",
            Side::Gui => "gui",
        }
    }
}

/// Compatibility version of the Lua extension contract.
///
/// This changes only when an existing API meaning or call shape is broken;
/// additive capabilities do not require a new version.
pub const API_VERSION: u32 = 1;

/// The git commit this was built from (`-dirty` with local changes), and the
/// checkout it was built in; empty when unknown.
pub const GIT_COMMIT: &str = env!("BONE_GIT_COMMIT");
pub const SOURCE_DIR: &str = env!("BONE_SOURCE_DIR");

static CORE_CAPABILITIES: &[&str] = &[
    "lua",
    "json",
    "modules",
    "fs",
    "core.config",
    "core.tools",
    "core.hooks",
    "core.providers",
    "core.jobs",
    "core.http_stream",
    "core.ask",
    "core.health",
    "core.ready",
    "core.reload",
    "core.hooks.system",
    "core.hooks.context",
    "core.hooks.errors",
    "core.hooks.stream",
    "core.hooks.session",
    "core.session_write",
    "core.queue",
    "core.model",
    "core.mcp",
    "core.rpc",
    "plugins.state",
    "docs",
];

static TUI_CAPABILITIES: &[&str] = &[
    "lua",
    "json",
    "modules",
    "fs",
    "tui.keymaps",
    "tui.input",
    "tui.commands",
    "tui.events",
    "tui.local_events",
    "tui.command_specs",
    "tui.dynamic_options",
    "tui.options",
    "tui.request",
    "tui.prompt",
    "tui.prompt_edit",
    "tui.chat_data",
    "tui.chat",
    "tui.windows",
    "tui.panels",
    "tui.regions",
    "tui.views",
    "tui.pickers",
    "tui.themes",
    "tui.jobs",
    "jobs.streaming",
    "tui.model",
    "tui.rpc",
    "plugins.state",
    "docs",
    "plugins.lifecycle",
    "tui.project",
    "tui.session",
    // The API shared with the GUI, by the same names there.
    "ui.options",
    "ui.events",
    "ui.commands",
    "ui.keymaps",
    "ui.themes",
    "ui.chat_data",
    "ui.views",
    "ui.request",
    "ui.layout",
    "ui.windows",
    "ui.panels",
    "ui.input",
    "ui.prompt",
    "ui.jobs",
    "ui.plugins",
    "ui.session",
    "ui.local_events",
    "ui.command_specs",
    "ui.dynamic_options",
    "ui.pickers",
    "ui.elements",
    "ui.model",
    "ui.rpc",
    "ui.project",
];

/// The GUI's: the shared UI API (`ui.*`), growing as more of it moves into
/// `bone-ui`.
static GUI_CAPABILITIES: &[&str] = &[
    "lua",
    "json",
    "modules",
    "fs",
    "plugins.state",
    "docs",
    "ui.options",
    "ui.events",
    "ui.commands",
    "ui.keymaps",
    "ui.themes",
    "ui.chat_data",
    "ui.views",
    "ui.request",
    "ui.windows",
    "ui.input",
    "ui.prompt",
    "ui.jobs",
    "ui.plugins",
    "ui.session",
    "ui.local_events",
    "ui.command_specs",
    "ui.dynamic_options",
    "ui.pickers",
    "ui.elements",
    "ui.model",
    "ui.rpc",
    "ui.project",
    "jobs.streaming",
    "plugins.lifecycle",
    "gui.nodes",
    "gui.window",
    "gui.notifications",
    "gui.dialogs",
    "gui.window_nodes",
];

/// Capabilities implemented by a Lua state on this side of bone.
pub fn capabilities(side: Side) -> &'static [&'static str] {
    match side {
        Side::Core => CORE_CAPABILITIES,
        Side::Tui => TUI_CAPABILITIES,
        Side::Gui => GUI_CAPABILITIES,
    }
}

fn capability_table(lua: &Lua, side: Side) -> mlua::Result<Table> {
    let table = lua.create_table_with_capacity(0, capabilities(side).len())?;
    for name in capabilities(side) {
        table.set(*name, true)?;
    }
    Ok(table)
}

/// `$BONE_CONFIG_DIR`, else `~/.bone` (`%USERPROFILE%\.bone` on Windows).
pub fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("BONE_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    sys::home_dir().unwrap_or_else(|| ".".into()).join(".bone")
}

/// Plugin directories: `<config dir>/plugins/<name>/`, in name order.
/// Names starting with `.` or `_` are skipped (rename to disable).
///
/// A plugin may contain `core.lua` (run in the core after the defaults and
/// before your `core.lua`), `tui.lua` (same, in the TUI), `lua/` (modules for
/// `require`) and `colors/` (colorschemes).
pub fn plugins(config_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(config_dir.join("plugins")) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .filter(|p| {
            !p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(['.', '_']))
        })
        .collect();
    dirs.sort();
    dirs
}

/// Run `file` of each plugin in `<config dir>/plugins/`, in name order,
/// leaving out the plugins `skip` names. While one runs,
/// `bone.plugin.current()` describes it. A plugin that fails does not stop
/// the rest: each failure is returned with the plugin's name, in load order.
pub fn run_user_plugins_each(
    lua: &Lua,
    config_dir: &Path,
    file: &str,
    skip: &dyn Fn(&str) -> bool,
) -> mlua::Result<Vec<(String, mlua::Error)>> {
    let mut failed = Vec::new();
    for plugin in plugins(config_dir) {
        let name = plugin_name(&plugin);
        if skip(&name) {
            continue;
        }
        set_loading(lua, Some(&plugin))?;
        let r = run_file(lua, &plugin.join(file));
        set_loading(lua, None)?;
        if let Err(e) = r {
            failed.push((name, e));
        }
    }
    Ok(failed)
}

/// A plugin's name: its folder's name.
pub fn plugin_name(dir: &Path) -> String {
    dir.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// Set `bone._loading` to `{ name, dir }` of the plugin being loaded.
pub fn set_loading(lua: &Lua, plugin: Option<&Path>) -> mlua::Result<()> {
    let bone: Table = lua.globals().get("bone")?;
    match plugin {
        Some(dir) => {
            let t = lua.create_table()?;
            t.set("name", plugin_name(dir))?;
            t.set("dir", dir.to_string_lossy().as_ref())?;
            t.set("kind", "plugin")?;
            bone.set("_loading", t)
        }
        None => bone.set("_loading", Value::Nil),
    }
}

/// A state name: letters, digits, `_`, `-` and `.`, not starting with `.`.
fn valid_state_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// Where `bone.state` keeps `name` on this side.
pub fn state_path(config_dir: &Path, side: Side, name: &str) -> PathBuf {
    config_dir
        .join("state")
        .join(side.name())
        .join(format!("{name}.json"))
}

/// `bone.state.load(name)` and `bone.state.save(name, value)`: JSON files
/// in `<config dir>/state/<side>/`, or memory without a config dir.
fn install_state(
    lua: &Lua,
    bone: &Table,
    side: Side,
    config_dir: Option<&Path>,
) -> mlua::Result<()> {
    let state = lua.create_table()?;
    let memory = lua.create_table()?;
    let check = |name: &str| -> mlua::Result<()> {
        if valid_state_name(name) {
            Ok(())
        } else {
            Err(mlua::Error::runtime(format!(
                "invalid state name {name:?} (letters, digits, _, - and .)"
            )))
        }
    };
    let dir = config_dir.map(Path::to_owned);
    let mem = memory.clone();
    state.set(
        "load",
        lua.create_function(move |lua, name: String| {
            check(&name)?;
            let text: Option<String> = match &dir {
                None => mem.get(name.as_str())?,
                Some(d) => match std::fs::read_to_string(state_path(d, side, &name)) {
                    Ok(t) => Some(t),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                    Err(e) => return Err(mlua::Error::runtime(format!("state {name}: {e}"))),
                },
            };
            match text {
                None => Ok(Value::Table(lua.create_table()?)),
                Some(t) => {
                    let v: serde_json::Value = serde_json::from_str(&t)
                        .map_err(|e| mlua::Error::runtime(format!("state {name}: {e}")))?;
                    to_lua(lua, &v)
                }
            }
        })?,
    )?;
    let dir = config_dir.map(Path::to_owned);
    state.set(
        "save",
        lua.create_function(move |_, (name, value): (String, Value)| {
            check(&name)?;
            let text =
                serde_json::to_string_pretty(&from_lua(&value)?).map_err(mlua::Error::external)?;
            let Some(d) = &dir else {
                return memory.set(name, text);
            };
            let path = state_path(d, side, &name);
            let write = || -> std::io::Result<()> {
                std::fs::create_dir_all(path.parent().expect("state dir"))?;
                // Write then rename, so a crash never leaves half a file.
                let tmp = path.with_extension("json.tmp");
                std::fs::write(&tmp, text)?;
                std::fs::rename(&tmp, &path)
            };
            write().map_err(|e| mlua::Error::runtime(format!("state {name}: {e}")))
        })?,
    )?;
    bone.set("state", state)
}

/// A fresh state with the shared `bone` table and module search set up.
/// `config_dir` is `None` for states that must not read user files (tests).
pub fn new_state(side: Side, config_dir: Option<&Path>) -> mlua::Result<Lua> {
    let lua = Lua::new();
    #[cfg(target_os = "android")]
    lua.load("jit.off()").exec()?;
    // os.getenv reads the process environment, as Rust sees it. The C
    // runtime's getenv on Windows keeps its own copy from startup, so it
    // would not see what bone sets later (HOME, from USERPROFILE).
    let os: Table = lua.globals().get("os")?;
    os.set(
        "getenv",
        lua.create_function(|_, name: String| Ok(std::env::var(name).ok()))?,
    )?;
    let bone = lua.create_table()?;
    bone.set("side", side.name())?;
    bone.set("version", env!("CARGO_PKG_VERSION"))?;
    let build = lua.create_table()?;
    build.set("commit", GIT_COMMIT)?;
    build.set("source", SOURCE_DIR)?;
    bone.set("build", build)?;
    bone.set("api_version", API_VERSION)?;
    bone.set("capabilities", capability_table(&lua, side)?)?;
    bone.set(
        "has_capability",
        lua.create_function(
            move |_, name: String| Ok(capabilities(side).contains(&name.as_str())),
        )?,
    )?;
    bone.set(
        "api_info",
        lua.create_function(move |lua, ()| {
            let info = lua.create_table()?;
            info.set("version", API_VERSION)?;
            info.set("side", side.name())?;
            // "linux", "macos", "windows"…: catalog packages name the ones they run on.
            info.set("os", std::env::consts::OS)?;
            info.set("capabilities", capability_table(lua, side)?)?;
            Ok(info)
        })?,
    )?;
    if let Some(dir) = config_dir {
        bone.set("config_dir", dir.to_string_lossy().as_ref())?;
    }
    lua.globals().set("bone", &bone)?;
    install_state(&lua, &bone, side, config_dir)?;
    // bone.fs.list(dir): { { name, type = "file" | "dir", size,
    // modified } }, sorted; nil and an error if it cannot be read. `size` is
    // in bytes and `modified` is Unix seconds; either may be absent when the
    // filesystem cannot provide that metadata.
    let fs = lua.create_table()?;
    fs.set(
        "list",
        lua.create_function(|lua, dir: String| {
            let dir = match dir.strip_prefix("~/") {
                Some(rest) => std::env::var_os("HOME")
                    .map(|h| PathBuf::from(h).join(rest))
                    .unwrap_or_else(|| PathBuf::from(&dir)),
                None => PathBuf::from(&dir),
            };
            let entries = match std::fs::read_dir(&dir) {
                Ok(e) => e,
                Err(e) => {
                    return mlua::IntoLuaMulti::into_lua_multi(
                        (Value::Nil, format!("{}: {e}", dir.display())),
                        lua,
                    );
                }
            };
            let mut list: Vec<(String, bool, Option<u64>, Option<u64>)> = entries
                .flatten()
                .map(|e| {
                    let metadata = e.metadata().ok();
                    let modified = metadata
                        .as_ref()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs());
                    (
                        e.file_name().to_string_lossy().into_owned(),
                        e.path().is_dir(),
                        metadata.as_ref().map(std::fs::Metadata::len),
                        modified,
                    )
                })
                .collect();
            list.sort();
            let out = lua.create_table()?;
            for (name, is_dir, size, modified) in list {
                let t = lua.create_table()?;
                t.set("name", name)?;
                t.set("type", if is_dir { "dir" } else { "file" })?;
                if let Some(size) = size {
                    t.set("size", size)?;
                }
                if let Some(modified) = modified {
                    t.set("modified", modified)?;
                }
                out.push(t)?;
            }
            mlua::IntoLuaMulti::into_lua_multi(out, lua)
        })?,
    )?;
    // bone.fs.mkdir(dir): make it and its parents. bone.fs.write(path, text):
    // replace a file whole (a temporary renamed over it), making its folder.
    // Both return true, or nil and an error.
    fs.set(
        "mkdir",
        lua.create_function(|lua, dir: String| {
            let r = std::fs::create_dir_all(expand_home(&dir));
            fs_result(lua, r.map_err(|e| format!("{dir}: {e}")))
        })?,
    )?;
    // Remove a directory and its contents without following symlinks.
    fs.set(
        "remove_tree",
        lua.create_function(|lua, dir: String| {
            let r = std::fs::remove_dir_all(expand_home(&dir));
            fs_result(lua, r.map_err(|e| format!("{dir}: {e}")))
        })?,
    )?;
    fs.set(
        "write",
        lua.create_function(|lua, (path, text): (String, mlua::String)| {
            let target = expand_home(&path);
            let r = (|| {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let name = target.file_name().unwrap_or_default().to_string_lossy();
                let tmp = target.with_file_name(format!(".{name}.tmp"));
                std::fs::write(&tmp, text.as_bytes())?;
                std::fs::rename(&tmp, &target)
            })();
            fs_result(lua, r.map_err(|e| format!("{path}: {e}")))
        })?,
    )?;
    bone.set("fs", fs)?;
    // bone.sha256(text): the hex SHA-256 of a string (e.g. to check a file).
    bone.set(
        "sha256",
        lua.create_function(|_, text: mlua::String| {
            Ok(ring::digest::digest(&ring::digest::SHA256, &text.as_bytes())
                .as_ref()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>())
        })?,
    )?;
    // bone.ui_libraries(): the UI library each app uses, and the user's own:
    // { tui = name, gui = name, mine = { { name, tui = bool, gui = bool } },
    // dir }. `standard` and `bare` are the built-in ones; `tui`/`gui` in
    // `mine` say whether that library has files for the app.
    if let Some(dir) = config_dir.map(Path::to_owned) {
        bone.set(
            "ui_libraries",
            lua.create_function(move |lua, ()| {
                let out = lua.create_table()?;
                let tui = tui_library(&dir).unwrap_or_else(|| "standard".into());
                let gui = library(&dir, Side::Gui).unwrap_or_else(|| "standard".into());
                out.set("tui", tui)?;
                out.set("gui", gui)?;
                let mine = lua.create_table()?;
                for name in tui_libraries(&dir) {
                    let lib = dir.join("libraries").join(&name);
                    let t = lua.create_table()?;
                    t.set("tui", lib.join("tui").is_dir())?;
                    t.set("gui", lib.join("gui").is_dir())?;
                    t.set("name", name)?;
                    mine.push(t)?;
                }
                out.set("mine", mine)?;
                out.set("dir", dir.join("libraries").to_string_lossy().as_ref())?;
                Ok(out)
            })?,
        )?;
    }
    // bone.docs: the customization guides. list() -> { { name, desc } } (the
    // description is the guide's first line); read(name) -> text, or nil and
    // an error.
    let docs = lua.create_table()?;
    docs.set(
        "list",
        lua.create_function(|lua, ()| {
            let out = lua.create_table()?;
            for (name, src) in DOCS {
                let entry = lua.create_table()?;
                entry.set("name", *name)?;
                entry.set(
                    "desc",
                    src.lines().find(|l| !l.trim().is_empty()).unwrap_or(""),
                )?;
                out.push(entry)?;
            }
            Ok(out)
        })?,
    )?;
    docs.set(
        "read",
        lua.create_function(
            |lua, name: String| match DOCS.iter().find(|(p, _)| *p == name) {
                Some((_, src)) => {
                    mlua::IntoLuaMulti::into_lua_multi(Value::String(lua.create_string(src)?), lua)
                }
                None => {
                    let mut known: Vec<&str> = DOCS.iter().map(|(p, _)| *p).collect();
                    known.sort();
                    mlua::IntoLuaMulti::into_lua_multi(
                        (
                            Value::Nil,
                            format!("no guide {name:?} (available: {})", known.join(", ")),
                        ),
                        lua,
                    )
                }
            },
        )?,
    )?;
    bone.set("docs", docs)?;

    let json = lua.create_table()?;
    json.set(
        "encode",
        lua.create_function(|lua, v: Value| {
            Ok(lua.from_value::<serde_json::Value>(v)?.to_string())
        })?,
    )?;
    json.set(
        "decode",
        lua.create_function(|lua, s: String| {
            to_lua(lua, &serde_json::from_str(&s).into_lua_err()?)
        })?,
    )?;
    // JSON `null` where a Lua `nil` would drop the key.
    json.set("null", lua.null())?;
    bone.set("json", json)?;

    setup_require(&lua, side, config_dir)?;
    let util: Table = lua.load("return require('bone.util')").eval()?;
    bone.set("inspect", util.get::<Function>("inspect")?)?;
    bone.set("util", util)?;
    Ok(lua)
}

/// JSON → Lua. `null` becomes `nil`; empty objects become empty tables, and
/// arrays carry mlua's array metatable, so that an empty one is still `[]`
/// when it goes back (`from_lua`, `bone.json.encode`).
pub fn to_lua(lua: &Lua, v: &serde_json::Value) -> mlua::Result<Value> {
    Ok(match v {
        serde_json::Value::Null => Value::Nil,
        serde_json::Value::Bool(b) => Value::Boolean(*b),
        serde_json::Value::Number(n) => match n.as_i64() {
            Some(i) => Value::Integer(i),
            None => Value::Number(n.as_f64().unwrap_or(0.0)),
        },
        serde_json::Value::String(s) => Value::String(lua.create_string(s)?),
        serde_json::Value::Array(a) => {
            let t = lua.create_table_with_capacity(a.len(), 0)?;
            for (i, v) in a.iter().enumerate() {
                t.raw_set(i + 1, to_lua(lua, v)?)?;
            }
            t.set_metatable(Some(lua.array_metatable()))?;
            Value::Table(t)
        }
        serde_json::Value::Object(o) => {
            let t = lua.create_table_with_capacity(0, o.len())?;
            for (k, v) in o {
                t.raw_set(k.as_str(), to_lua(lua, v)?)?;
            }
            Value::Table(t)
        }
    })
}

/// Lua → JSON. Tables with keys 1..n are arrays; an empty table is `{}`,
/// unless it came from a JSON array (and was not rebuilt on the way).
pub fn from_lua(v: &Value) -> mlua::Result<serde_json::Value> {
    walk(v, 0)
}

/// [`from_lua`] at nesting `depth`; past JSON's usual 128 levels the table
/// is taken to contain itself (`t.self = t`) rather than overflow the stack.
fn walk(v: &Value, depth: u32) -> mlua::Result<serde_json::Value> {
    if depth > 128 {
        return Err(mlua::Error::runtime("table nests too deeply (does it contain itself?)"));
    }
    Ok(match v {
        Value::Nil => serde_json::Value::Null,
        Value::Boolean(b) => serde_json::Value::Bool(*b),
        Value::Integer(i) => serde_json::Value::from(*i),
        Value::Number(n) => {
            if n.fract() == 0.0 && n.abs() < 9.0e15 {
                serde_json::Value::from(*n as i64)
            } else {
                serde_json::Value::from(*n)
            }
        }
        Value::String(s) => serde_json::Value::String(s.to_str()?.to_owned()),
        Value::Table(t) => {
            let len = t.raw_len();
            let count = t.clone().pairs::<Value, Value>().count();
            // Only mlua can tell its mark: it writes such a table as a list.
            let marked = || matches!(serde_json::to_value(t), Ok(serde_json::Value::Array(_)));
            if len == count && (len > 0 || marked()) {
                let mut a = Vec::with_capacity(len);
                for i in 1..=len {
                    a.push(walk(&t.raw_get::<Value>(i)?, depth + 1)?);
                }
                serde_json::Value::Array(a)
            } else {
                let mut o = serde_json::Map::new();
                for pair in t.clone().pairs::<Value, Value>() {
                    let (k, v) = pair?;
                    let key = match k {
                        Value::String(s) => s.to_str()?.to_owned(),
                        Value::Integer(i) => i.to_string(),
                        other => {
                            return Err(mlua::Error::runtime(format!(
                                "cannot use a {} as a JSON key",
                                other.type_name()
                            )));
                        }
                    };
                    o.insert(key, walk(&v, depth + 1)?);
                }
                serde_json::Value::Object(o)
            }
        }
        Value::LightUserData(u) if u.0.is_null() => serde_json::Value::Null,
        other => {
            return Err(mlua::Error::runtime(format!(
                "cannot convert a {} to JSON",
                other.type_name()
            )));
        }
    })
}

/// The source of a built-in runtime file and its chunk name. Built-in files
/// always come from the binary: the config dir cannot replace them (a
/// library, see [`frontend_runtime_source`], is the place for changes).
fn runtime_source(rel: &str) -> Option<(String, String)> {
    builtin_source(rel).map(|src| (src.to_owned(), format!("runtime/{rel}")))
}

/// The TUI library chosen with `/tui`, kept in `<config dir>/library`:
/// `None` for the standard one, else `bare` (no standard UI) or the name of
/// a copy under `<config dir>/libraries/`.
pub fn tui_library(config_dir: &Path) -> Option<String> {
    let name = std::fs::read_to_string(config_dir.join("library")).ok()?;
    let name = name.trim();
    (!name.is_empty() && name != "standard").then(|| name.to_owned())
}

/// The GUI's own library, chosen with `/gui use` and kept in
/// `<config dir>/gui-library`: `None` when none was chosen (the GUI then
/// follows `/tui`, see [`library`]), else `standard`, `bare`, or a copy
/// under `libraries/`.
pub fn gui_library(config_dir: &Path) -> Option<String> {
    let name = std::fs::read_to_string(config_dir.join("gui-library")).ok()?;
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

/// Choose the GUI's library: `standard`, `bare` or a copy under
/// `libraries/` that has GUI files.
pub fn set_gui_library(config_dir: &Path, name: &str) -> Result<(), String> {
    let known = ["standard", "bare"].contains(&name)
        || config_dir.join("libraries").join(name).join("gui").is_dir();
    if !known {
        return Err(format!("no GUI library named {name}"));
    }
    let path = config_dir.join("gui-library");
    std::fs::write(&path, format!("{name}\n")).map_err(|e| format!("{}: {e}", path.display()))
}

/// The library frontend `side` draws with (`None`: the standard one). The
/// TUI's is the one chosen with `/tui`. The GUI's is the one chosen with
/// `/gui use`; without one it takes the TUI's if that is `bare` or has
/// files for the GUI (a `gui/` directory), and is standard otherwise, so a
/// library made for the terminal leaves the GUI its own.
pub fn library(config_dir: &Path, side: Side) -> Option<String> {
    if side == Side::Gui
        && let Some(name) = gui_library(config_dir)
    {
        return (name != "standard").then_some(name);
    }
    let name = tui_library(config_dir)?;
    match side {
        Side::Tui => Some(name),
        Side::Gui if name == "bare" => Some(name),
        Side::Gui
            if config_dir
                .join("libraries")
                .join(&name)
                .join("gui")
                .is_dir() =>
        {
            Some(name)
        }
        Side::Gui | Side::Core => None,
    }
}

/// The TUI's runtime file `rel`: the chosen library's copy first; a file the
/// library lacks is the built-in one.
pub fn tui_runtime_source(config_dir: Option<&Path>, rel: &str) -> Option<(String, String)> {
    frontend_runtime_source(config_dir, Side::Tui, rel)
}

/// Frontend `side`'s runtime file `rel`: its library's copy (see
/// [`library`]) first, else the built-in one.
pub fn frontend_runtime_source(
    config_dir: Option<&Path>,
    side: Side,
    rel: &str,
) -> Option<(String, String)> {
    if let Some(dir) = config_dir
        && let Some(name) = library(dir, side)
    {
        let path = dir.join("libraries").join(name).join(rel);
        if let Ok(src) = std::fs::read_to_string(&path) {
            return Some((src, path.to_string_lossy().into_owned()));
        }
    }
    runtime_source(rel)
}

/// The libraries under `<config dir>/libraries/`, by name.
pub fn tui_libraries(config_dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(config_dir.join("libraries"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        // `standard` and `bare` are always the built-in ones: a folder of
        // that name is never one of yours.
        .filter(|n| !n.starts_with('.') && n != "standard" && n != "bare")
        .collect();
    names.sort();
    names
}

/// The entry file `app` (`"tui"` or `"gui"`) of a new library: it runs the
/// built-in UI it starts from (`base`: `""` for standard, `"bare"`), always
/// the version of bone in use, and the library's changes go below it.
pub fn library_entry(app: &str, base: &str) -> String {
    let file = if base == "bare" { "bare" } else { "defaults" };
    format!(
        "-- This library starts from bone's built-in {ui} UI, run by the line below\n\
         -- (always the version of bone in use). Put your changes after it.\n\
         bone.builtin(\"{app}/{file}.lua\")\n",
        ui = if base == "bare" { "bare" } else { "standard" },
    )
}

/// Make a new library, `<config dir>/libraries/<name>/`, for frontend
/// `side`, that starts from the standard UI (`base` `""`) or the bare one
/// (`"bare"`). It holds only that frontend's entry file (see
/// [`library_entry`]), never copies of built-in files, so it keeps getting
/// bone's updates. An existing library is never touched.
pub fn create_tui_library(
    config_dir: &Path,
    name: &str,
    base: &str,
    side: Side,
) -> Result<PathBuf, String> {
    let valid = !name.starts_with('.')
        && !["", "bare", "standard"].contains(&name)
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-.".contains(c));
    if !valid {
        return Err(format!("bad library name {name:?}"));
    }
    if !["", "bare"].contains(&base) {
        return Err(format!("no library to start from named {base}"));
    }
    let root = config_dir.join("libraries");
    let dest = root.join(name);
    if dest.exists() {
        return Err(format!("{} already exists", dest.display()));
    }
    // Made in a hidden folder first, so a half-made one is never listed.
    let tmp = root.join(format!(".{name}.tmp"));
    let make = || -> std::io::Result<()> {
        let _ = std::fs::remove_dir_all(&tmp);
        let app = if side == Side::Gui { "gui" } else { "tui" };
        std::fs::create_dir_all(tmp.join(app))?;
        std::fs::write(tmp.join(app).join("defaults.lua"), library_entry(app, base))?;
        std::fs::rename(&tmp, &dest)
    };
    make().map_err(|e| {
        let _ = std::fs::remove_dir_all(&tmp);
        format!("{}: {e}", dest.display())
    })?;
    Ok(dest)
}

/// The files under `dir`, as paths relative to it (`/`-separated).
fn files_under(dir: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else if let Ok(rel) = path.strip_prefix(root) {
                let parts: Vec<_> = rel.iter().map(|p| p.to_string_lossy()).collect();
                out.push(parts.join("/"));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// Remove the empty folders under `dir` (not `dir` itself).
fn remove_empty_dirs(dir: &Path) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            remove_empty_dirs(&path);
            let _ = std::fs::remove_dir(&path);
        }
    }
}

/// Delete a library that is not in use: it is moved aside to a hidden
/// `libraries/.deleted-<name>-<time>/`, returned.
pub fn delete_tui_library(config_dir: &Path, name: &str) -> Result<PathBuf, String> {
    if !tui_libraries(config_dir).iter().any(|n| n == name) {
        return Err(format!("no library named {name}"));
    }
    if tui_library(config_dir).as_deref() == Some(name)
        || gui_library(config_dir).as_deref() == Some(name)
    {
        return Err(format!("{name} is in use; switch to another first"));
    }
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let root = config_dir.join("libraries");
    let to = root.join(format!(".deleted-{name}-{secs}"));
    std::fs::rename(root.join(name), &to).map_err(|e| format!("{}: {e}", to.display()))?;
    Ok(to)
}

/// Choose the TUI library: `standard`, `bare` or one under `libraries/`.
pub fn set_tui_library(config_dir: &Path, name: &str) -> Result<(), String> {
    let path = config_dir.join("library");
    let r = match name {
        "standard" => match std::fs::remove_file(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            r => r,
        },
        "bare" => std::fs::write(&path, "bare\n"),
        _ if tui_libraries(config_dir).iter().any(|n| n == name) => {
            std::fs::write(&path, format!("{name}\n"))
        }
        _ => return Err(format!("no library named {name}")),
    };
    r.map_err(|e| format!("{}: {e}", path.display()))
}

/// What `<config dir>/docs/builtin/README.md` says.
const BUILTIN_README: &str = "\
# bone's built-in files, to read

These are copies of the Lua files built into bone (its `runtime/` folder),
rewritten at every start so they always match the bone in use. bone never
loads them: editing them changes nothing, and the edit is lost at the next
start. To change something, see `../customizing.md`: changes go in a UI
library of your own, starting from `bone.builtin(...)`.
";

/// Write the embedded docs to `<config dir>/docs/`, and the built-in
/// runtime files to read to `docs/builtin/`, so they always match this
/// build. A file is only rewritten when it changed; one no longer built in
/// is removed from `docs/builtin/`.
pub fn write_docs(config_dir: &Path) -> std::io::Result<()> {
    let builtin = config_dir.join("docs").join("builtin");
    let docs = DOCS.iter().map(|(name, src)| (config_dir.join("docs").join(name), *src));
    let runtime = RUNTIME.iter().map(|(rel, src)| (builtin.join(rel), *src));
    let readme = std::iter::once((builtin.join("README.md"), BUILTIN_README));
    for (path, src) in docs.chain(runtime).chain(readme) {
        if std::fs::read_to_string(&path).is_ok_and(|old| old == src) {
            continue;
        }
        std::fs::create_dir_all(path.parent().expect("a file's directory"))?;
        std::fs::write(&path, src)?;
    }
    for rel in files_under(&builtin) {
        if rel != "README.md" && builtin_source(&rel).is_none() {
            let _ = std::fs::remove_file(builtin.join(&rel));
        }
    }
    remove_empty_dirs(&builtin);
    Ok(())
}

/// `~/x` as a path under $HOME.
fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join(rest))
            .unwrap_or_else(|| PathBuf::from(path)),
        None => PathBuf::from(path),
    }
}

/// true, or nil and the error, for Lua.
fn fs_result(lua: &Lua, r: Result<(), String>) -> mlua::Result<mlua::MultiValue> {
    match r {
        Ok(()) => mlua::IntoLuaMulti::into_lua_multi(true, lua),
        Err(e) => mlua::IntoLuaMulti::into_lua_multi((Value::Nil, e), lua),
    }
}

/// The built-in runtime file at `rel`, ignoring any override.
pub fn builtin_source(rel: &str) -> Option<&'static str> {
    RUNTIME.iter().find(|(p, _)| *p == rel).map(|(_, src)| *src)
}

/// Run a built-in runtime file.
pub fn run_runtime(lua: &Lua, rel: &str) -> mlua::Result<()> {
    let (src, name) = runtime_source(rel)
        .ok_or_else(|| mlua::Error::runtime(format!("missing runtime file {rel}")))?;
    lua.load(src).set_name(format!("@{name}")).exec()
}

/// Run a user file if it exists. Returns whether it did.
pub fn run_file(lua: &Lua, path: &Path) -> mlua::Result<bool> {
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => {
            return Err(mlua::Error::external(format!(
                "cannot read {}: {e}",
                path.display()
            )));
        }
    };
    lua.load(src)
        .set_name(format!("@{}", path.display()))
        .exec()?;
    Ok(true)
}

fn setup_require(lua: &Lua, side: Side, config_dir: Option<&Path>) -> mlua::Result<()> {
    let package: Table = lua.globals().get("package")?;
    // User modules come first, through the standard path searcher.
    if let Some(dir) = config_dir {
        let mut prefix = String::new();
        let mut names: Vec<String> = Vec::new();
        for root in std::iter::once(dir.to_owned()).chain(plugins(dir)) {
            let d = root.join("lua");
            let d = d.to_string_lossy();
            prefix.push_str(&format!("{d}/?.lua;{d}/?/init.lua;"));
            if root != dir {
                names.push(
                    root.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
        let path: String = package.get("path")?;
        package.set("path", format!("{prefix}{path}"))?;
        lua.globals().get::<Table>("bone")?.set("plugins", names)?;
    } else {
        lua.globals()
            .get::<Table>("bone")?
            .set("plugins", lua.create_table()?)?;
    }
    // The runtime `lua/` directory (the library's copy, then the built-in
    // one) is searched before those paths, so a built-in module always comes
    // from the binary or the library in use, never a stray user file.
    let dir = config_dir.map(Path::to_owned);
    let searcher = lua.create_function(move |lua, name: String| {
        let rel = name.replace('.', "/");
        for candidate in [format!("lua/{rel}.lua"), format!("lua/{rel}/init.lua")] {
            let found = match side {
                Side::Tui | Side::Gui => frontend_runtime_source(dir.as_deref(), side, &candidate),
                _ => runtime_source(&candidate),
            };
            if let Some((src, chunk_name)) = found {
                let f = lua
                    .load(src)
                    .set_name(format!("@{chunk_name}"))
                    .into_function()?;
                return Ok(Value::Function(f));
            }
        }
        Ok(Value::String(lua.create_string(format!(
            "\n\tno runtime module '{name}'"
        ))?))
    })?;
    // bone.builtin(name): the built-in version of a runtime module (or file,
    // given as a path), even when the library in use has its own copy, so
    // a library can start from it and change only what it needs.
    let builtin = lua.create_function(|lua, name: String| {
        let candidates = if name.contains('/') {
            vec![name.clone()]
        } else {
            let rel = name.replace('.', "/");
            vec![format!("lua/{rel}.lua"), format!("lua/{rel}/init.lua")]
        };
        let Some((rel, src)) = candidates
            .iter()
            .find_map(|c| builtin_source(c).map(|s| (c.clone(), s)))
        else {
            return Err(mlua::Error::runtime(format!(
                "no built-in runtime file for {name:?}"
            )));
        };
        lua.load(src)
            .set_name(format!("@runtime/{rel} (built-in)"))
            .call::<MultiValue>(name)
    })?;
    lua.globals()
        .get::<Table>("bone")?
        .set("builtin", builtin)?;
    // LuaJIT (5.1) calls the list `loaders`; 5.2+ calls it `searchers`.
    let list: Table = package
        .get("loaders")
        .or_else(|_| package.get("searchers"))?;
    // Second, after `package.preload`.
    for i in (2..=list.raw_len()).rev() {
        list.raw_set(i + 1, list.raw_get::<Value>(i)?)?;
    }
    list.raw_set(2, searcher)?;
    Ok(())
}

/// First line of a Lua error, for one-line display.
pub fn short_error(e: &mlua::Error) -> String {
    let full = e.to_string();
    let first = full.lines().next().unwrap_or("").trim();
    first
        .strip_prefix("runtime error: ")
        .unwrap_or(first)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_round_trip_and_helpers() {
        let lua = new_state(Side::Tui, None).unwrap();
        let out: String = lua
            .load(r#"return bone.json.encode(bone.json.decode('{"a":[1,2,{"b":null}],"c":1.5,"d":{}}'))"#)
            .eval()
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v, serde_json::json!({"a": [1, 2, {}], "c": 1.5, "d": {}}));
        let null: String = lua
            .load("return bone.json.encode({ x = bone.json.null })")
            .eval()
            .unwrap();
        assert_eq!(null, r#"{"x":null}"#);
        // An empty array stays one through Lua, also once emptied in place;
        // a table made in Lua is an object until it has items, and so is
        // one that was given names.
        let lists: Value = lua
            .load(
                r#"local t = bone.json.decode('{"a":[],"b":{},"c":[1],"d":[[]],"g":[]}')
                table.remove(t.c)
                t.e, t.f, t.g.k = {}, { "x" }, 1
                return t"#,
            )
            .eval()
            .unwrap();
        assert_eq!(
            from_lua(&lists).unwrap(),
            serde_json::json!({ "a": [], "b": {}, "c": [], "d": [[]], "e": {}, "f": ["x"], "g": { "k": 1 } })
        );
        let encoded: String = lua
            .load(r#"return bone.json.encode(bone.json.decode('{"a":[],"b":{}}'))"#)
            .eval()
            .unwrap();
        // Compared as values: the order of keys in the text follows Lua's
        // table order when serde_json keeps insertion order (a feature other
        // crates in a build may turn on).
        let encoded: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(encoded, serde_json::json!({ "a": [], "b": {} }));
        let hash: String = lua.load(r#"return bone.sha256("abc")"#).eval().unwrap();
        assert_eq!(
            hash,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let s: String = lua
            .load(r#"return bone.inspect({1, "x", k = {true}})"#)
            .eval()
            .unwrap();
        assert_eq!(s, "{\n  1,\n  \"x\",\n  k = {\n    true\n  }\n}");
        let parts: Vec<String> = lua
            .load(r#"return bone.util.split("a.b..c", ".")"#)
            .eval()
            .unwrap();
        assert_eq!(parts, ["a", "b", "", "c"]);
        assert_eq!(
            lua.load("return bone.side").eval::<String>().unwrap(),
            "tui"
        );
    }

    #[test]
    fn fs_list_includes_file_metadata() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("note.txt"), b"abc").unwrap();
        std::fs::create_dir(dir.path().join("folder")).unwrap();
        let lua = new_state(Side::Tui, None).unwrap();
        let path = dir.path().to_string_lossy();
        let (name, size, modified, kind): (String, u64, u64, String) = lua
            .load(format!(
                "local x = bone.fs.list({path:?}); return x[2].name, x[2].size, x[2].modified, x[2].type"
            ))
            .eval()
            .unwrap();
        assert_eq!(name, "note.txt");
        assert_eq!(size, 3);
        assert!(modified > 0);
        assert_eq!(kind, "file");
    }

    #[test]
    fn state_persists_per_side_and_plugins_know_their_name() {
        let dir = tempfile::tempdir().unwrap();
        let lua = new_state(Side::Core, Some(dir.path())).unwrap();
        lua.load(r#"bone.state.save("x", { a = 1, list = { "b" } })"#)
            .exec()
            .unwrap();
        let text = std::fs::read_to_string(dir.path().join("state/core/x.json")).unwrap();
        assert!(text.contains("\"a\": 1"), "{text}");
        let a: i64 = lua.load("return bone.state.load('x').a").eval().unwrap();
        assert_eq!(a, 1);
        let empty: bool = lua
            .load("return next(bone.state.load('missing')) == nil")
            .eval()
            .unwrap();
        assert!(empty);
        assert!(lua.load("bone.state.save('../x', {})").exec().is_err());
        let tui = new_state(Side::Tui, Some(dir.path())).unwrap();
        let own: Value = tui.load("return bone.state.load('x').a").eval().unwrap();
        assert!(own.is_nil());
        // Without a config dir, state lives in memory.
        let mem = new_state(Side::Tui, None).unwrap();
        let v: i64 = mem
            .load("bone.state.save('y', { n = 3 }); return bone.state.load('y').n")
            .eval()
            .unwrap();
        assert_eq!(v, 3);

        std::fs::create_dir_all(dir.path().join("plugins/p1")).unwrap();
        std::fs::write(
            dir.path().join("plugins/p1/core.lua"),
            "seen = bone._loading.name",
        )
        .unwrap();
        assert!(run_user_plugins_each(&lua, dir.path(), "core.lua", &|_| false).unwrap().is_empty());
        let seen: String = lua.load("return seen").eval().unwrap();
        assert_eq!(seen, "p1");
        let after: Value = lua.load("return bone._loading").eval().unwrap();
        assert!(after.is_nil());
    }

    #[test]
    fn require_finds_user_modules_but_builtins_stay_built_in() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("lua/my")).unwrap();
        std::fs::write(dir.path().join("lua/my/mod.lua"), "return { x = 1 }").unwrap();
        // A user module does not replace a built-in one.
        std::fs::create_dir_all(dir.path().join("lua/bone")).unwrap();
        std::fs::write(
            dir.path().join("lua/bone/util.lua"),
            "return { inspect = function() return 'mine' end }",
        )
        .unwrap();
        let lua = new_state(Side::Core, Some(dir.path())).unwrap();
        assert_eq!(
            lua.load("return require('my.mod').x")
                .eval::<i64>()
                .unwrap(),
            1
        );
        assert_eq!(
            lua.load("return bone.inspect(1)").eval::<String>().unwrap(),
            "1"
        );
        let err = lua.load("require('nope.nothing')").exec().unwrap_err();
        assert!(
            err.to_string().contains("no runtime module 'nope.nothing'"),
            "{err}"
        );
    }
}

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

pub mod wait;

use std::path::{Path, PathBuf};

use mlua::{Function, Lua, LuaSerdeExt, Table, Value};

/// Files from the repository's `runtime/` directory, by relative path.
macro_rules! runtime_files {
    ($($path:literal),* $(,)?) => {
        &[$(($path, include_str!(concat!("../../../runtime/", $path)))),*]
    };
}

pub const RUNTIME: &[(&str, &str)] = runtime_files![
    "lua/bone/util.lua",
    "tui/api.lua",
    "tui/defaults.lua",
    "core/api.lua",
    "core/defaults.lua",
    "colors/black.lua",
    "colors/ansi.lua",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Core,
    Tui,
}

impl Side {
    pub fn name(self) -> &'static str {
        match self {
            Side::Core => "core",
            Side::Tui => "tui",
        }
    }
}

/// Compatibility version of the Lua extension contract.
///
/// This changes only when an existing API meaning or call shape is broken;
/// additive capabilities do not require a new version.
pub const API_VERSION: u32 = 1;

static CORE_CAPABILITIES: &[&str] = &[
    "lua",
    "json",
    "modules",
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
    "plugins.state",
];

static TUI_CAPABILITIES: &[&str] = &[
    "lua",
    "json",
    "modules",
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
    "plugins.state",
    "plugins.lifecycle",
    "tui.project",
    "tui.session",
];

/// Capabilities implemented by a Lua state on this side of bone.
pub fn capabilities(side: Side) -> &'static [&'static str] {
    match side {
        Side::Core => CORE_CAPABILITIES,
        Side::Tui => TUI_CAPABILITIES,
    }
}

fn capability_table(lua: &Lua, side: Side) -> mlua::Result<Table> {
    let table = lua.create_table_with_capacity(0, capabilities(side).len())?;
    for name in capabilities(side) {
        table.set(*name, true)?;
    }
    Ok(table)
}

/// `$BONE_CONFIG_DIR`, else `~/.bone`.
pub fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("BONE_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| ".".into());
    PathBuf::from(home).join(".bone")
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

/// Run `file` of each plugin in `<config dir>/plugins/`, in name order.
/// While one runs, `bone.plugin.current()` describes it.
pub fn run_user_plugins(lua: &Lua, config_dir: &Path, file: &str) -> mlua::Result<()> {
    run_user_plugins_except(lua, config_dir, file, &|_| false)
}

/// [`run_user_plugins`], leaving out the plugins `skip` names.
pub fn run_user_plugins_except(
    lua: &Lua,
    config_dir: &Path,
    file: &str,
    skip: &dyn Fn(&str) -> bool,
) -> mlua::Result<()> {
    for plugin in plugins(config_dir) {
        if skip(&plugin_name(&plugin)) {
            continue;
        }
        set_loading(lua, Some(&plugin))?;
        let r = run_file(lua, &plugin.join(file));
        set_loading(lua, None)?;
        r?;
    }
    Ok(())
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
    state_file(config_dir, side.name(), name)
}

fn state_file(config_dir: &Path, scope: &str, name: &str) -> PathBuf {
    config_dir
        .join("state")
        .join(scope)
        .join(format!("{name}.json"))
}

/// `opts.shared`: the store both sides read (`state/shared/`), for a
/// plugin's core and TUI halves.
fn state_scope(side: Side, opts: &Option<Table>) -> mlua::Result<&'static str> {
    let shared = match opts {
        Some(o) => o.get::<Option<bool>>("shared")?.unwrap_or(false),
        None => false,
    };
    Ok(if shared { "shared" } else { side.name() })
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
        lua.create_function(move |lua, (name, opts): (String, Option<Table>)| {
            check(&name)?;
            let scope = state_scope(side, &opts)?;
            let text: Option<String> = match &dir {
                None => mem.get(format!("{scope}/{name}"))?,
                Some(d) => match std::fs::read_to_string(state_file(d, scope, &name)) {
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
        lua.create_function(
            move |_, (name, value, opts): (String, Value, Option<Table>)| {
                check(&name)?;
                let scope = state_scope(side, &opts)?;
                let text = serde_json::to_string_pretty(&from_lua(&value)?)
                    .map_err(mlua::Error::external)?;
                let Some(d) = &dir else {
                    return memory.set(format!("{scope}/{name}"), text);
                };
                let path = state_file(d, scope, &name);
                let write = || -> std::io::Result<()> {
                    std::fs::create_dir_all(path.parent().expect("state dir"))?;
                    // Write then rename, so a crash never leaves half a file.
                    let tmp = path.with_extension("json.tmp");
                    std::fs::write(&tmp, text)?;
                    std::fs::rename(&tmp, &path)
                };
                write().map_err(|e| mlua::Error::runtime(format!("state {name}: {e}")))
            },
        )?,
    )?;
    bone.set("state", state)
}

/// A fresh state with the shared `bone` table and module search set up.
/// `config_dir` is `None` for states that must not read user files (tests).
pub fn new_state(side: Side, config_dir: Option<&Path>) -> mlua::Result<Lua> {
    let lua = Lua::new();
    let bone = lua.create_table()?;
    bone.set("side", side.name())?;
    bone.set("version", env!("CARGO_PKG_VERSION"))?;
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
            info.set("capabilities", capability_table(lua, side)?)?;
            Ok(info)
        })?,
    )?;
    if let Some(dir) = config_dir {
        bone.set("config_dir", dir.to_string_lossy().as_ref())?;
    }
    lua.globals().set("bone", &bone)?;
    install_state(&lua, &bone, side, config_dir)?;

    let json = lua.create_table()?;
    json.set(
        "encode",
        lua.create_function(|lua, v: Value| {
            let v: serde_json::Value = lua.from_value(v)?;
            Ok(v.to_string())
        })?,
    )?;
    json.set(
        "decode",
        lua.create_function(|lua, s: String| {
            let v: serde_json::Value = serde_json::from_str(&s).map_err(mlua::Error::external)?;
            to_lua(lua, &v)
        })?,
    )?;
    bone.set("json", json)?;

    setup_require(&lua, config_dir)?;
    let util: Table = lua.load("return require('bone.util')").eval()?;
    bone.set("inspect", util.get::<Function>("inspect")?)?;
    bone.set("util", util)?;
    Ok(lua)
}

/// JSON → Lua. `null` becomes `nil`; empty objects become empty tables.
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

/// Lua → JSON. Tables with keys 1..n are arrays; an empty table is `{}`.
pub fn from_lua(v: &Value) -> mlua::Result<serde_json::Value> {
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
            if len > 0 && len == count {
                let mut a = Vec::with_capacity(len);
                for i in 1..=len {
                    a.push(from_lua(&t.raw_get::<Value>(i)?)?);
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
                    o.insert(key, from_lua(&v)?);
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

/// The source of a runtime file: the user's override if present, else the
/// embedded copy.
pub fn runtime_source(config_dir: Option<&Path>, rel: &str) -> Option<(String, String)> {
    if let Some(dir) = config_dir {
        let path = dir.join("runtime").join(rel);
        if let Ok(src) = std::fs::read_to_string(&path) {
            return Some((src, path.to_string_lossy().into_owned()));
        }
    }
    RUNTIME
        .iter()
        .find(|(p, _)| *p == rel)
        .map(|(_, src)| ((*src).to_owned(), format!("runtime/{rel}")))
}

/// Run a runtime file (see [`runtime_source`]).
pub fn run_runtime(lua: &Lua, config_dir: Option<&Path>, rel: &str) -> mlua::Result<()> {
    let (src, name) = runtime_source(config_dir, rel)
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

fn setup_require(lua: &Lua, config_dir: Option<&Path>) -> mlua::Result<()> {
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
    // Then the embedded runtime `lua/` directory.
    let dir = config_dir.map(Path::to_owned);
    let searcher = lua.create_function(move |lua, name: String| {
        let rel = name.replace('.', "/");
        for candidate in [format!("lua/{rel}.lua"), format!("lua/{rel}/init.lua")] {
            if let Some((src, chunk_name)) = runtime_source(dir.as_deref(), &candidate) {
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
    // LuaJIT (5.1) calls the list `loaders`; 5.2+ calls it `searchers`.
    let list: Table = package
        .get("loaders")
        .or_else(|_| package.get("searchers"))?;
    list.raw_set(list.raw_len() + 1, searcher)?;
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
        lua.load("bone.state.save('x', { s = 1 }, { shared = true })")
            .exec()
            .unwrap();
        assert!(dir.path().join("state/shared/x.json").is_file());
        let tui = new_state(Side::Tui, Some(dir.path())).unwrap();
        let (own, shared): (Value, i64) = tui
            .load("return bone.state.load('x').a, bone.state.load('x', { shared = true }).s")
            .eval()
            .unwrap();
        assert!(own.is_nil());
        assert_eq!(shared, 1);
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
        run_user_plugins(&lua, dir.path(), "core.lua").unwrap();
        let seen: String = lua.load("return seen").eval().unwrap();
        assert_eq!(seen, "p1");
        let after: Value = lua.load("return bone._loading").eval().unwrap();
        assert!(after.is_nil());
    }

    #[test]
    fn api_contract_reports_version_and_capabilities() {
        for (side, present, absent) in [
            (Side::Core, "core.tools", "tui.keymaps"),
            (Side::Tui, "tui.keymaps", "core.tools"),
        ] {
            let lua = new_state(side, None).unwrap();
            let bone: Table = lua.globals().get("bone").unwrap();
            assert_eq!(bone.get::<u32>("api_version").unwrap(), API_VERSION);

            let capabilities: Table = bone.get("capabilities").unwrap();
            assert!(capabilities.get::<bool>(present).unwrap());
            assert_eq!(capabilities.get::<Option<bool>>(absent).unwrap(), None);

            let info: Table = lua.load("return bone.api_info()").eval().unwrap();
            assert_eq!(info.get::<u32>("version").unwrap(), API_VERSION);
            assert_eq!(info.get::<String>("side").unwrap(), side.name());
            assert!(
                info.get::<Table>("capabilities")
                    .unwrap()
                    .get::<bool>(present)
                    .unwrap()
            );

            let has: bool = lua
                .load(format!("return bone.has_capability({present:?})"))
                .eval()
                .unwrap();
            assert!(has);
            let has: bool = lua
                .load(format!("return bone.has_capability({absent:?})"))
                .eval()
                .unwrap();
            assert!(!has);
        }
    }

    #[test]
    fn require_prefers_user_modules_and_runtime_overrides() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("lua/my")).unwrap();
        std::fs::write(dir.path().join("lua/my/mod.lua"), "return { x = 1 }").unwrap();
        std::fs::create_dir_all(dir.path().join("runtime/lua/bone")).unwrap();
        std::fs::write(
            dir.path().join("runtime/lua/bone/util.lua"),
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
            "mine"
        );
        let err = lua.load("require('nope.nothing')").exec().unwrap_err();
        assert!(
            err.to_string().contains("no runtime module 'nope.nothing'"),
            "{err}"
        );
    }
}

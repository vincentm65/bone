//! Plugin lifecycle in every frontend: loading, ownership of what a plugin
//! creates, shutdown hooks, unloading and reloading, and a trusted
//! project's config.
//!
//! While a plugin's files run (`ui.lua`, then the frontend's own:
//! `tui.lua` or `gui.lua`), and whenever one of its callbacks runs, the
//! owner is that plugin, so the keymaps, commands, event handlers, panels,
//! windows, jobs, options, raw key interceptors and contexts it creates are
//! recorded as its own. Unloading runs its `on_shutdown` hooks, saves its
//! `bone.plugin.state` tables, removes all of that, and forgets the modules
//! it `require`d, so a reload starts fresh. Other changes (views,
//! highlights, `bone.ui` fields) are the plugin's to undo in a shutdown hook.

use std::path::{Path, PathBuf};

use mlua::{Function, IntoLuaMulti, Lua, MultiValue, Table, Value};
use serde_json::{Value as Json, json};

use super::{Host, Owned, args, drop_callback, err, fire, json as ret_json, ret, store_callback};
use crate::keymap::Context;
use crate::keys::{self, Key};

/// Owner name of a trusted project's `.bone/` config.
pub const PROJECT: &str = "project";
/// The trusted project roots, in the TUI's `bone.state` (where they have
/// always been kept): one list for every frontend.
const TRUSTED: &str = "trusted-projects";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Plugin,
    Project,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Plugin => "plugin",
            Kind::Project => "project",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Plugin {
    pub name: String,
    pub dir: PathBuf,
    pub kind: Kind,
    pub loaded: bool,
    /// The last load's error.
    pub error: Option<String>,
    /// Modules first `require`d while it loaded.
    modules: Vec<String>,
}

impl Plugin {
    pub fn info(&self) -> Json {
        let (core, frontends) = parts(&self.dir);
        json!({
            "name": self.name,
            "dir": self.dir.to_string_lossy(),
            "kind": self.kind.name(),
            "loaded": self.loaded,
            "error": self.error,
            "core": core,
            "frontends": frontends,
        })
    }
}

/// What a plugin folder has: a core half, and the frontends it supports
/// (`ui.lua` is both; `tui.lua` and `gui.lua` one each).
pub fn parts(dir: &Path) -> (bool, Vec<&'static str>) {
    let has = |f: &str| dir.join(f).is_file();
    let shared = has("ui.lua");
    let frontends = [("tui", "tui.lua"), ("gui", "gui.lua")]
        .into_iter()
        .filter(|(_, f)| shared || has(f))
        .map(|(side, _)| side)
        .collect();
    (has("core.lua"), frontends)
}

/// The known plugins, and the `bone.plugin.on_shutdown` hooks by owner.
pub struct PluginStore<'a> {
    pub plugins: &'a mut Vec<Plugin>,
    pub shutdown_hooks: &'a mut Vec<(Option<String>, u64)>,
}

pub(super) fn dispatch<H: Host>(
    h: &mut H,
    lua: &Lua,
    op: &str,
    a: MultiValue,
) -> Result<mlua::Result<MultiValue>, MultiValue> {
    Ok(match op {
        "plugin_current" => {
            let Some(name) = h.owners().owner.clone() else {
                return Ok(ret(lua, Value::Nil));
            };
            let info = plugin(h, &name)
                .map(|p| p.info())
                .unwrap_or_else(|| json!({ "name": name }));
            ret_json(lua, &info)
        }
        "plugin_list" => {
            let list: Vec<Json> = h.plugin_store().plugins.iter().map(Plugin::info).collect();
            ret_json(lua, &Json::Array(list))
        }
        "plugin_load" | "plugin_unload" | "plugin_reload" => (|| {
            let name: String = args(lua, a)?;
            if h.owners().owner.as_deref() == Some(name.as_str()) && op != "plugin_load" {
                return Err(err(format!(
                    "plugin {name} cannot unload itself while it runs"
                )));
            }
            h.plugin_files_changing();
            match op {
                "plugin_load" => load_plugin_by_name(h, &name),
                "plugin_unload" => unload_plugin(h, &name),
                _ => reload_plugin(h, &name),
            }
            .map_err(err)?;
            ret(lua, ())
        })(),
        "plugin_on_shutdown" => args::<Function>(lua, a).and_then(|f| {
            let cb = store_callback(h, lua, f)?;
            let owner = h.owners().owner.clone();
            h.plugin_store().shutdown_hooks.push((owner, cb));
            ret(lua, ())
        }),
        "project_trust" => args::<bool>(lua, a)
            .and_then(|on| set_project_trust(h, on).map_err(err))
            .and_then(|()| ret(lua, ())),
        "project_info" => match project_info(h) {
            Some(info) => ret_json(lua, &info),
            None => ret(lua, Value::Nil),
        },
        _ => return Err(a),
    })
}

pub fn plugin<'a, H: Host>(h: &'a mut H, name: &str) -> Option<&'a Plugin> {
    h.plugin_store().plugins.iter().find(|p| p.name == name)
}

/// Names in `package.loaded`.
fn loaded_modules(lua: &Lua) -> mlua::Result<Vec<String>> {
    let loaded: Table = lua.globals().get::<Table>("package")?.get("loaded")?;
    loaded
        .pairs::<Value, Value>()
        .filter_map(|p| match p {
            Ok((Value::String(k), _)) => Some(Ok(k.to_string_lossy())),
            Ok(_) => None,
            Err(e) => Some(Err(e)),
        })
        .collect()
}

/// Call `bone._api(op, ...)` from Rust, as Lua would.
pub fn api_call<H: Host>(h: &mut H, op: &str, args: impl IntoLuaMulti) -> mlua::Result<MultiValue> {
    h.with_api(|lua| {
        let api: Function = lua.globals().get::<Table>("bone")?.get("_api")?;
        let mut all = MultiValue::new();
        all.push_back(Value::String(lua.create_string(op)?));
        all.extend(args.into_lua_multi(lua)?);
        api.call::<MultiValue>(all)
    })
}

/// Forget stored callbacks (outside a Lua call).
pub fn release_callbacks<H: Host>(h: &mut H, ids: &[u64]) {
    if ids.is_empty() {
        return;
    }
    let _ = h.with_api(|lua| {
        for id in ids {
            drop_callback(lua, *id)?;
        }
        Ok(())
    });
}

/// The plugin folder `name` in the config dir (loaded or not).
fn plugin_dir<H: Host>(h: &H, name: &str) -> Option<PathBuf> {
    let dir = h.config_dir()?.join("plugins").join(name);
    dir.is_dir().then_some(dir)
}

/// Know about a plugin without loading it (it is switched off).
pub fn register_plugin<H: Host>(h: &mut H, name: &str, dir: &Path, kind: Kind) {
    let plugins = &mut *h.plugin_store().plugins;
    if !plugins.iter().any(|p| p.name == name) {
        plugins.push(Plugin {
            name: name.to_owned(),
            dir: dir.to_owned(),
            kind,
            loaded: false,
            error: None,
            modules: Vec::new(),
        });
    }
}

/// Run a plugin's files for this frontend as that plugin. Errors are
/// reported and kept in its info; what it set up before failing stays (and
/// unloads).
pub fn load_plugin<H: Host>(h: &mut H, name: &str, dir: &Path, kind: Kind) {
    register_plugin(h, name, dir, kind);
    // Its modules come before the rest (they may be new since startup).
    let lua_dir = dir.join("lua");
    let _ = h.with_api(|lua| {
        let package: Table = lua.globals().get("package")?;
        let path: String = package.get("path")?;
        let d = lua_dir.to_string_lossy();
        let entry = format!("{d}/?.lua;{d}/?/init.lua;");
        if !path.contains(&entry) {
            package.set("path", format!("{entry}{path}"))?;
        }
        Ok(())
    });
    let before = h.with_api(loaded_modules).unwrap_or_default();
    let prev = h.owners().owner.replace(name.to_owned());
    let mut error = None;
    for file in h.config_files().iter().map(|f| dir.join(f)) {
        if let Err(e) = h.with_api(|lua| bone_lua::run_file(lua, &file)) {
            h.lua_error(&file.display().to_string(), &e);
            error = Some(bone_lua::short_error(&e));
            break;
        }
    }
    *h.owners().owner = prev;
    let after = h.with_api(loaded_modules).unwrap_or_default();
    // Its README is a help page while it is loaded.
    if error.is_none()
        && let Ok(text) = std::fs::read_to_string(dir.join("README.md"))
    {
        let _ = h.with_api(|lua| {
            let doc = lua.create_table()?;
            doc.set("name", name)?;
            doc.set("text", text)?;
            doc.set("plugin", name)?;
            lua.globals()
                .get::<Table>("bone")?
                .get::<Table>("_docs")?
                .push(doc)
        });
        h.owners()
            .owned
            .entry(name.to_owned())
            .or_default()
            .push(Owned::Doc);
    }
    let p = h
        .plugin_store()
        .plugins
        .iter_mut()
        .find(|p| p.name == name)
        .expect("plugin entry");
    p.loaded = true;
    p.error = error;
    p.modules = after.into_iter().filter(|m| !before.contains(m)).collect();
    let info = p.info();
    fire(h, "plugin/loaded", info);
    h.changed();
}

/// Load a plugin from `<config dir>/plugins/<name>/` that is not loaded.
pub fn load_plugin_by_name<H: Host>(h: &mut H, name: &str) -> Result<(), String> {
    if plugin(h, name).is_some_and(|p| p.loaded) {
        return Err(format!("plugin {name} is already loaded"));
    }
    if let Some(dir) = plugin(h, name)
        .filter(|p| p.kind == Kind::Project)
        .map(|p| p.dir.clone())
    {
        load_plugin(h, name, &dir, Kind::Project);
        return Ok(());
    }
    let dir =
        plugin_dir(h, name).ok_or_else(|| format!("no plugin {name} in the plugins folder"))?;
    load_plugin(h, name, &dir, Kind::Plugin);
    Ok(())
}

/// Run `name`'s shutdown hooks, save its state, and remove everything it
/// created.
pub fn unload_plugin<H: Host>(h: &mut H, name: &str) -> Result<(), String> {
    if !plugin(h, name).is_some_and(|p| p.loaded) {
        return Err(format!("plugin {name} is not loaded"));
    }
    run_shutdown(h, Some(name));
    for what in h.owners().owned.remove(name).unwrap_or_default() {
        remove_owned(h, name, what);
    }
    let keys: Vec<(Context, Vec<Key>)> = (h.keymap_store().owner.iter())
        .filter(|(_, o)| o.as_str() == name)
        .map(|(k, _)| k.clone())
        .collect();
    for (ctx, seq) in keys {
        let key = seq.iter().map(keys::format).collect::<Vec<_>>().join(" ");
        let opts = h.with_api(|lua| {
            let t = lua.create_table()?;
            t.set("context", ctx.name())?;
            Ok(t)
        });
        if let Ok(opts) = opts {
            let _ = api_call(h, "keymap_del", (key, opts));
        }
        h.keymap_store().owner.remove(&(ctx, seq));
    }
    let modules = (h.plugin_store().plugins.iter_mut())
        .find(|p| p.name == name)
        .map(|p| std::mem::take(&mut p.modules))
        .unwrap_or_default();
    let _ = h.with_api(|lua| {
        let loaded: Table = lua.globals().get::<Table>("package")?.get("loaded")?;
        for m in &modules {
            loaded.set(m.as_str(), Value::Nil)?;
        }
        Ok(())
    });
    // Callbacks it still owns belong to things that are gone.
    let gone: Vec<u64> = (h.owners().callback_owner.iter())
        .filter(|(_, o)| o.as_str() == name)
        .map(|(cb, _)| *cb)
        .collect();
    release_callbacks(h, &gone);
    h.owners().callback_owner.retain(|_, o| o != name);
    let p = (h.plugin_store().plugins.iter_mut())
        .find(|p| p.name == name)
        .expect("loaded plugin");
    p.loaded = false;
    let info = p.info();
    fire(h, "plugin/unloaded", info);
    h.changed();
    Ok(())
}

pub fn reload_plugin<H: Host>(h: &mut H, name: &str) -> Result<(), String> {
    unload_plugin(h, name)?;
    load_plugin_by_name(h, name)
}

fn remove_owned<H: Host>(h: &mut H, plugin: &str, what: Owned) {
    // Each removal goes through the same path as the Lua API, after
    // checking the thing is still the plugin's.
    let r = match what {
        Owned::Command(name, cb) => {
            if h.user_commands().get(&name).map(|c| c.callback) == Some(cb) {
                api_call(h, "command_del", name).map(drop)
            } else {
                Ok(())
            }
        }
        Owned::Autocmd(id) => api_call(h, "off", id).map(drop),
        Owned::Panel(id, seq) => {
            let ours =
                super::panels::panel(h.panel_store().panels, &id).is_some_and(|p| p.seq == seq);
            if ours {
                api_call(h, "panel_close", id).map(drop)
            } else {
                Ok(())
            }
        }
        Owned::Popup(id) => api_call(h, "popup_close", id).map(drop),
        Owned::Job(id) => {
            // It ends without calling back into the unloaded plugin.
            let cbs = h.cancel_job(id);
            release_callbacks(h, &cbs);
            Ok(())
        }
        Owned::Option(name) => {
            let ours = (h.option_store().dynamic.get(&name))
                .is_some_and(|o| o.owner.as_deref() == Some(plugin));
            if ours {
                api_call(h, "opt_del", name).map(drop)
            } else {
                Ok(())
            }
        }
        Owned::Doc => h.with_api(|lua| {
            let docs: Table = lua.globals().get::<Table>("bone")?.get("_docs")?;
            for i in (1..=docs.raw_len()).rev() {
                let owner: Option<String> = docs.get::<Table>(i)?.get("plugin")?;
                if owner.as_deref() == Some(plugin) {
                    docs.raw_remove(i)?;
                }
            }
            Ok(())
        }),
        Owned::Raw(id) => api_call(h, "keymap_raw_del", id).map(drop),
        Owned::Context(ctx) => {
            if h.keymap_store().keymaps.has_context(&ctx) {
                api_call(h, "keymap_context_del", ctx.name().to_owned()).map(drop)
            } else {
                Ok(())
            }
        }
    };
    if let Err(e) = r {
        h.lua_error("unloading a plugin", &e);
    }
}

/// Run the shutdown hooks of `owner` (`None`: every one, the user's config
/// last) and save the `bone.plugin.state` tables they opened.
pub fn run_shutdown<H: Host>(h: &mut H, owner: Option<&str>) {
    let mut hooks: Vec<(Option<String>, u64)> = Vec::new();
    h.plugin_store().shutdown_hooks.retain(|(o, cb)| {
        let run = owner.is_none() || o.as_deref() == owner;
        if run {
            hooks.push((o.clone(), *cb));
        }
        !run
    });
    if owner.is_none() {
        // Plugins in reverse load order, then the user's config.
        let order: Vec<String> = (h.plugin_store().plugins.iter())
            .map(|p| p.name.clone())
            .collect();
        hooks.sort_by_key(|(o, _)| match o {
            None => usize::MAX,
            Some(o) => order.len() - order.iter().position(|n| n == o).unwrap_or(0),
        });
    }
    for (_, cb) in &hooks {
        h.call_callback(*cb, "shutdown", |_| Ok(Value::Nil));
    }
    let ids: Vec<u64> = hooks.iter().map(|h| h.1).collect();
    release_callbacks(h, &ids);
    save_plugin_states(h, owner);
}

/// Save the `bone.plugin.state` tables of `owner` (every one for `None`)
/// without shutting anything down.
pub fn save_plugin_states<H: Host>(h: &mut H, owner: Option<&str>) {
    let owner = owner.map(str::to_owned);
    let r = h.with_api(|lua| {
        let bone: Table = lua.globals().get("bone")?;
        match bone.get::<Option<Function>>("_save_states")? {
            Some(f) => f.call::<()>(owner),
            None => Ok(()),
        }
    });
    if let Err(e) = r {
        h.lua_error("saving plugin state", &e);
    }
}

// ---- project config ----------------------------------------------------------

/// The trusted project's `.bone/`, while its config is registered (loaded,
/// or failed to load).
pub fn project_dir<H: Host>(h: &mut H) -> Option<PathBuf> {
    plugin(h, PROJECT)
        .filter(|p| p.kind == Kind::Project)
        .map(|p| p.dir.clone())
}

/// The nearest directory at or above the working directory with a
/// `.bone/` config this frontend runs (that is not the config dir itself).
pub fn project_root<H: Host>(h: &H) -> Option<PathBuf> {
    let config = h.config_dir().and_then(|d| std::fs::canonicalize(d).ok());
    let cwd = std::fs::canonicalize(h.cwd()).ok()?;
    cwd.ancestors()
        .find(|a| {
            let d = a.join(".bone");
            h.config_files().iter().any(|f| d.join(f).is_file())
                && std::fs::canonicalize(&d).ok() != config
        })
        .map(Path::to_owned)
}

fn trusted_projects<H: Host>(h: &H) -> Vec<String> {
    let Some(dir) = h.config_dir() else {
        return Vec::new();
    };
    std::fs::read_to_string(bone_lua::state_path(dir, bone_lua::Side::Tui, TRUSTED))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn set_trusted<H: Host>(h: &H, root: &Path, trusted: bool) -> Result<(), String> {
    let dir = h
        .config_dir()
        .ok_or("there is no config dir to remember trust in")?;
    let root = root.to_string_lossy().into_owned();
    let mut list = trusted_projects(h);
    list.retain(|r| *r != root);
    if trusted {
        list.push(root);
    }
    let path = bone_lua::state_path(dir, bone_lua::Side::Tui, TRUSTED);
    let write = || -> std::io::Result<()> {
        std::fs::create_dir_all(path.parent().expect("state dir"))?;
        std::fs::write(&path, serde_json::to_string_pretty(&list)?)
    };
    write().map_err(|e| format!("cannot save trusted projects: {e}"))
}

/// `{ root, file, trusted, loaded }`, or `None` outside a project. `file` is
/// the first config file it has for this frontend.
pub fn project_info<H: Host>(h: &mut H) -> Option<Json> {
    let root = project_root(h)?;
    let trusted = trusted_projects(h).contains(&root.to_string_lossy().into_owned());
    let file = (h.config_files().iter())
        .map(|f| root.join(".bone").join(f))
        .find(|p| p.is_file())
        .unwrap_or_else(|| root.join(".bone").join(h.config_files()[0]));
    let loaded = plugin(h, PROJECT).is_some_and(|p| p.loaded);
    Some(json!({
        "root": root.to_string_lossy(),
        "file": file.to_string_lossy(),
        "trusted": trusted,
        "loaded": loaded,
    }))
}

/// At startup, after the user's config: load a trusted project's config, or
/// say that there is one.
pub fn load_project<H: Host>(h: &mut H) {
    let Some(info) = project_info(h) else {
        return;
    };
    if info["trusted"] == true {
        let dir = PathBuf::from(info["root"].as_str().unwrap_or_default()).join(".bone");
        load_plugin(h, PROJECT, &dir, Kind::Project);
    } else {
        h.info(format!(
            "{} is not loaded: /plugins trust runs it",
            info["file"].as_str().unwrap_or_default()
        ));
    }
}

/// Trust this directory's project config (and load it), or stop trusting it
/// (and unload it): `bone.project.trust(on)`.
pub fn set_project_trust<H: Host>(h: &mut H, trusted: bool) -> Result<(), String> {
    let root = project_root(h).ok_or("no .bone/ config here or in a parent directory")?;
    set_trusted(h, &root, trusted)?;
    let loaded = plugin(h, PROJECT).is_some_and(|p| p.loaded);
    if trusted && !loaded {
        load_plugin(h, PROJECT, &root.join(".bone"), Kind::Project);
    } else if !trusted && loaded {
        unload_plugin(h, PROJECT)?;
    }
    Ok(())
}

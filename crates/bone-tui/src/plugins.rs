//! Plugin lifecycle in the TUI: loading, ownership of what a plugin creates,
//! shutdown hooks, unloading and reloading, and a trusted project's config.
//!
//! While a plugin's `tui.lua` runs, and whenever one of its callbacks runs,
//! the app's `owner` is that plugin, so the keymaps, commands, event
//! handlers, panels, windows, jobs, options, raw key interceptors and
//! contexts it creates are recorded as its own. Unloading runs its
//! `on_shutdown` hooks, saves its `bone.plugin.state` tables, removes all of
//! that, and forgets the modules it `require`d, so a reload starts fresh.
//! Other changes (views, highlights, `bone.ui` fields) are the plugin's to
//! undo in a shutdown hook.

use std::path::{Path, PathBuf};

use mlua::{IntoLuaMulti, MultiValue, Table, Value};
use serde_json::{Value as Json, json};

use crate::app::App;
use crate::keymap::Context;

/// Owner name of a trusted project's `.bone/tui.lua`.
pub const PROJECT: &str = "project";
/// The trusted project roots, in `bone.state` (TUI side).
const TRUSTED: &str = "trusted-projects";

/// Something a plugin created, to remove when it unloads.
#[derive(Debug, Clone, PartialEq)]
pub enum Owned {
    /// A user command, with its callback (it may have been redefined since).
    Command(String, u64),
    Autocmd(u64),
    /// A panel id, with its sequence number (the id may be reused).
    Panel(String, u64),
    Popup(u64),
    Job(u64),
    Option(String),
    Raw(u64),
    Context(Context),
}

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
        json!({
            "name": self.name,
            "dir": self.dir.to_string_lossy(),
            "kind": self.kind.name(),
            "loaded": self.loaded,
            "error": self.error,
        })
    }
}

/// Names in `package.loaded`.
fn loaded_modules(lua: &mlua::Lua) -> mlua::Result<Vec<String>> {
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

impl App {
    /// Record that the current owner (if a plugin) created `what`.
    pub fn own(&mut self, what: Owned) {
        if let Some(owner) = &self.owner {
            self.owned.entry(owner.clone()).or_default().push(what);
        }
    }

    pub fn plugin(&self, name: &str) -> Option<&Plugin> {
        self.plugins.iter().find(|p| p.name == name)
    }

    /// Call `bone._api(op, ...)` from Rust, as Lua would.
    pub fn api_call(&mut self, op: &str, args: impl IntoLuaMulti) -> mlua::Result<MultiValue> {
        self.with_api(|lua| {
            let api: mlua::Function = lua.globals().get::<Table>("bone")?.get("_api")?;
            let mut all = MultiValue::new();
            all.push_back(Value::String(lua.create_string(op)?));
            all.extend(args.into_lua_multi(lua)?);
            api.call::<MultiValue>(all)
        })
    }

    /// The plugin folder `name` in the config dir (loaded or not).
    fn plugin_dir(&self, name: &str) -> Option<PathBuf> {
        let dir = self.config_dir.as_ref()?.join("plugins").join(name);
        dir.is_dir().then_some(dir)
    }

    /// Run a plugin's `tui.lua` as that plugin. Errors are reported and kept
    /// in its info; what it set up before failing stays (and unloads).
    /// Know about a plugin without loading it (it is switched off).
    pub fn register_plugin(&mut self, name: &str, dir: &Path, kind: Kind) {
        if !self.plugins.iter().any(|p| p.name == name) {
            self.plugins.push(Plugin {
                name: name.to_owned(),
                dir: dir.to_owned(),
                kind,
                loaded: false,
                error: None,
                modules: Vec::new(),
            });
        }
    }

    pub fn load_plugin(&mut self, name: &str, dir: &Path, kind: Kind) {
        let file = dir.join("tui.lua");
        if !self.plugins.iter().any(|p| p.name == name) {
            self.plugins.push(Plugin {
                name: name.to_owned(),
                dir: dir.to_owned(),
                kind,
                loaded: false,
                error: None,
                modules: Vec::new(),
            });
        }
        // Its modules come before the rest (they may be new since startup).
        let lua_dir = dir.join("lua");
        let _ = self.with_api(|lua| {
            let package: Table = lua.globals().get("package")?;
            let path: String = package.get("path")?;
            let d = lua_dir.to_string_lossy();
            let entry = format!("{d}/?.lua;{d}/?/init.lua;");
            if !path.contains(&entry) {
                package.set("path", format!("{entry}{path}"))?;
            }
            Ok(())
        });
        let before = self.with_api(loaded_modules).unwrap_or_default();
        let prev = self.owner.replace(name.to_owned());
        let r = self.with_api(|lua| bone_lua::run_file(lua, &file));
        self.owner = prev;
        let after = self.with_api(loaded_modules).unwrap_or_default();
        let error = match r {
            Ok(_) => None,
            Err(e) => {
                self.lua_error(&file.display().to_string(), &e);
                Some(bone_lua::short_error(&e))
            }
        };
        let p = self
            .plugins
            .iter_mut()
            .find(|p| p.name == name)
            .expect("plugin entry");
        p.loaded = true;
        p.error = error;
        p.modules = after.into_iter().filter(|m| !before.contains(m)).collect();
        let info = p.info();
        self.fire("plugin/loaded", info);
        self.dirty = true;
    }

    /// Load a plugin from `<config dir>/plugins/<name>/` that is not loaded.
    pub fn load_plugin_by_name(&mut self, name: &str) -> Result<(), String> {
        if self.plugin(name).is_some_and(|p| p.loaded) {
            return Err(format!("plugin {name} is already loaded"));
        }
        if let Some(p) = self.plugin(name).filter(|p| p.kind == Kind::Project) {
            let dir = p.dir.clone();
            self.load_plugin(name, &dir, Kind::Project);
            return Ok(());
        }
        let dir = self
            .plugin_dir(name)
            .ok_or_else(|| format!("no plugin {name} in the plugins folder"))?;
        self.load_plugin(name, &dir, Kind::Plugin);
        Ok(())
    }

    /// Run `name`'s shutdown hooks, save its state, and remove everything
    /// it created.
    pub fn unload_plugin(&mut self, name: &str) -> Result<(), String> {
        if !self.plugin(name).is_some_and(|p| p.loaded) {
            return Err(format!("plugin {name} is not loaded"));
        }
        self.run_shutdown(Some(name));
        for what in self.owned.remove(name).unwrap_or_default() {
            self.remove_owned(name, what);
        }
        let keys: Vec<(Context, Vec<crate::keys::Key>)> = self
            .keymap_owner
            .iter()
            .filter(|(_, o)| o.as_str() == name)
            .map(|(k, _)| k.clone())
            .collect();
        for (ctx, seq) in keys {
            let key = seq
                .iter()
                .map(crate::keys::format)
                .collect::<Vec<_>>()
                .join(" ");
            let opts = self.with_api(|lua| {
                let t = lua.create_table()?;
                t.set("context", ctx.name())?;
                Ok(t)
            });
            if let Ok(opts) = opts {
                let _ = self.api_call("keymap_del", (key, opts));
            }
            self.keymap_owner.remove(&(ctx, seq));
        }
        let modules = self
            .plugins
            .iter_mut()
            .find(|p| p.name == name)
            .map(|p| std::mem::take(&mut p.modules))
            .unwrap_or_default();
        let _ = self.with_api(|lua| {
            let loaded: Table = lua.globals().get::<Table>("package")?.get("loaded")?;
            for m in &modules {
                loaded.set(m.as_str(), Value::Nil)?;
            }
            Ok(())
        });
        // Callbacks it still owns belong to things that are gone.
        let gone: Vec<u64> = self
            .callback_owner
            .iter()
            .filter(|(_, o)| o.as_str() == name)
            .map(|(cb, _)| *cb)
            .collect();
        self.release_callbacks(&gone);
        self.callback_owner.retain(|_, o| o != name);
        let p = self
            .plugins
            .iter_mut()
            .find(|p| p.name == name)
            .expect("loaded plugin");
        p.loaded = false;
        let info = p.info();
        self.fire("plugin/unloaded", info);
        self.dirty = true;
        Ok(())
    }

    pub fn reload_plugin(&mut self, name: &str) -> Result<(), String> {
        self.unload_plugin(name)?;
        self.load_plugin_by_name(name)
    }

    fn remove_owned(&mut self, plugin: &str, what: Owned) {
        // Each removal goes through the same path as the Lua API, after
        // checking the thing is still the plugin's.
        let r = match what {
            Owned::Command(name, cb) => {
                if self.user_commands.get(&name).map(|c| c.callback) == Some(cb) {
                    self.api_call("command_del", name).map(drop)
                } else {
                    Ok(())
                }
            }
            Owned::Autocmd(id) => self.api_call("off", id).map(drop),
            Owned::Panel(id, seq) => {
                if self.panel(&id).is_some_and(|p| p.seq == seq) {
                    self.api_call("panel_close", id).map(drop)
                } else {
                    Ok(())
                }
            }
            Owned::Popup(id) => self.api_call("popup_close", id).map(drop),
            Owned::Job(id) => {
                // It ends without calling back into the unloaded plugin.
                self.cancel_job(id);
                let cbs = self.jobs.forget_callbacks(id);
                self.release_callbacks(&cbs);
                Ok(())
            }
            Owned::Option(name) => {
                let ours = self
                    .dynamic_options
                    .get(&name)
                    .is_some_and(|o| o.owner.as_deref() == Some(plugin));
                if ours {
                    self.api_call("opt_del", name).map(drop)
                } else {
                    Ok(())
                }
            }
            Owned::Raw(id) => self.api_call("keymap_raw_del", id).map(drop),
            Owned::Context(ctx) => {
                if self.keymaps.has_context(&ctx) {
                    self.api_call("keymap_context_del", ctx.name().to_owned())
                        .map(drop)
                } else {
                    Ok(())
                }
            }
        };
        if let Err(e) = r {
            self.lua_error("unloading a plugin", &e);
        }
    }

    /// Run the shutdown hooks of `owner` (`None`: every one, the user's
    /// config last) and save the `bone.plugin.state` tables they opened.
    pub fn run_shutdown(&mut self, owner: Option<&str>) {
        let mut hooks: Vec<(Option<String>, u64)> = Vec::new();
        self.shutdown_hooks.retain(|(o, cb)| {
            let run = owner.is_none() || o.as_deref() == owner;
            if run {
                hooks.push((o.clone(), *cb));
            }
            !run
        });
        if owner.is_none() {
            // Plugins in reverse load order, then the user's config.
            let order: Vec<String> = self.plugins.iter().map(|p| p.name.clone()).collect();
            hooks.sort_by_key(|(o, _)| match o {
                None => usize::MAX,
                Some(o) => order.len() - order.iter().position(|n| n == o).unwrap_or(0),
            });
        }
        for (_, cb) in &hooks {
            self.call_callback(*cb, "shutdown", |_| Ok(Value::Nil));
        }
        let ids: Vec<u64> = hooks.iter().map(|h| h.1).collect();
        self.release_callbacks(&ids);
        self.save_plugin_states(owner);
    }

    /// Save the `bone.plugin.state` tables of `owner` (every one for
    /// `None`) without shutting anything down.
    pub fn save_plugin_states(&mut self, owner: Option<&str>) {
        let owner = owner.map(str::to_owned);
        let r = self.with_api(|lua| {
            let bone: Table = lua.globals().get("bone")?;
            match bone.get::<Option<mlua::Function>>("_save_states")? {
                Some(f) => f.call::<()>(owner),
                None => Ok(()),
            }
        });
        if let Err(e) = r {
            self.lua_error("saving plugin state", &e);
        }
    }

    /// Before quitting: every plugin's shutdown hooks and state.
    pub fn shutdown(&mut self) {
        self.run_shutdown(None);
    }

    // ---- project config ------------------------------------------------------

    /// The trusted project's `.bone/`, while its config is registered
    /// (loaded, or failed to load).
    pub fn project_dir(&self) -> Option<PathBuf> {
        self.plugin(PROJECT)
            .filter(|p| p.kind == Kind::Project)
            .map(|p| p.dir.clone())
    }

    /// A core Lua file changed: reload the core's configuration. The core
    /// keeps the old one if the new one fails to load.
    pub fn reload_core_config(&mut self) {
        self.request::<bone_proto::methods::CoreReload>(bone_proto::methods::Empty {}, |app, r| {
            match r {
                Ok(r) if r.warnings.is_empty() => app.info("core configuration reloaded"),
                Ok(r) => app.info(format!(
                    "core configuration reloaded: {}",
                    r.warnings.join("; ")
                )),
                Err(e) => app.error(format!(
                    "core reload failed, kept the previous configuration: {e}"
                )),
            }
        });
    }

    /// The nearest directory at or above the working directory with a
    /// `.bone/tui.lua` (that is not the config dir itself).
    pub fn project_root(&self) -> Option<PathBuf> {
        let config = self
            .config_dir
            .as_ref()
            .and_then(|d| std::fs::canonicalize(d).ok());
        let cwd = std::fs::canonicalize(&self.cwd).ok()?;
        cwd.ancestors()
            .find(|a| {
                let d = a.join(".bone");
                d.join("tui.lua").is_file() && std::fs::canonicalize(&d).ok() != config
            })
            .map(Path::to_owned)
    }

    fn trusted_projects(&self) -> Vec<String> {
        let Some(dir) = &self.config_dir else {
            return Vec::new();
        };
        std::fs::read_to_string(bone_lua::state_path(dir, bone_lua::Side::Tui, TRUSTED))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    fn set_trusted(&mut self, root: &Path, trusted: bool) -> Result<(), String> {
        let dir = self
            .config_dir
            .clone()
            .ok_or("there is no config dir to remember trust in")?;
        let root = root.to_string_lossy().into_owned();
        let mut list = self.trusted_projects();
        list.retain(|r| *r != root);
        if trusted {
            list.push(root);
        }
        let path = bone_lua::state_path(&dir, bone_lua::Side::Tui, TRUSTED);
        let write = || -> std::io::Result<()> {
            std::fs::create_dir_all(path.parent().expect("state dir"))?;
            std::fs::write(&path, serde_json::to_string_pretty(&list)?)
        };
        write().map_err(|e| format!("cannot save trusted projects: {e}"))
    }

    /// `{ root, file, trusted, loaded }`, or `None` outside a project.
    pub fn project_info(&self) -> Option<Json> {
        let root = self.project_root()?;
        let trusted = self
            .trusted_projects()
            .contains(&root.to_string_lossy().into_owned());
        Some(json!({
            "root": root.to_string_lossy(),
            "file": root.join(".bone/tui.lua").to_string_lossy(),
            "trusted": trusted,
            "loaded": self.plugin(PROJECT).is_some_and(|p| p.loaded),
        }))
    }

    /// At startup, after the user's config: load a trusted project's
    /// config, or say that there is one.
    pub fn load_project(&mut self) {
        let Some(info) = self.project_info() else {
            return;
        };
        if info["trusted"] == true {
            let dir = PathBuf::from(info["root"].as_str().unwrap_or_default()).join(".bone");
            self.load_plugin(PROJECT, &dir, Kind::Project);
        } else {
            self.info(format!(
                "{} is not loaded: /plugins trust runs it",
                info["file"].as_str().unwrap_or_default()
            ));
        }
    }

    /// Trust this directory's project config (and load it), or stop
    /// trusting it (and unload it): `bone.project.trust(on)`.
    pub fn set_project_trust(&mut self, trusted: bool) -> Result<(), String> {
        let root = self
            .project_root()
            .ok_or("no .bone/tui.lua here or in a parent directory")?;
        self.set_trusted(&root, trusted)?;
        let loaded = self.plugin(PROJECT).is_some_and(|p| p.loaded);
        if trusted && !loaded {
            self.load_plugin(PROJECT, &root.join(".bone"), Kind::Project);
        } else if !trusted && loaded {
            self.unload_plugin(PROJECT)?;
        }
        Ok(())
    }
}

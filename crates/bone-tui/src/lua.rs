//! The TUI's Lua state and the `bone._api` bridge.
//!
//! Lua code only ever runs inside [`App::with_api`], which lends the whole
//! `App` to a scoped dispatch function for the duration of the call. The
//! persistent `bone._api(op, ...)` forwards to whichever dispatcher is
//! current, so API calls act on live state and nested calls (a mapping that
//! runs a command that calls Lua) reborrow safely.

use std::path::PathBuf;

use bone_lua::{Side, short_error, to_lua};
use bone_proto::methods::{ProcessCancel, ProcessRef};
use mlua::{Function, Lua, MultiValue, Table, Value};

use crate::app::App;
use crate::keymap::{Action, Builtin, Context};
use crate::keys;
use bone_ui::lua::{CALLBACKS, args, context_from_name, err, opt, ret};
use serde_json::Value as Json;

/// What a plugin, the user's config, or a project runs in the TUI, in
/// order: the UI code it shares with the GUI, then the TUI's own.
pub const CONFIG_FILES: [&str; 2] = ["ui.lua", "tui.lua"];

use bone_ui::lua::canonical_event;
pub use bone_ui::lua::{Autocmd, UserCommand};

fn popup_event(p: &crate::app::Popup) -> serde_json::Value {
    serde_json::json!({
        "id": p.id,
        "kind": "popup",
        "focus": p.focus,
        "anchor": p.anchor,
        "z": p.z,
        "width": p.width,
        "height": p.height,
        "row": p.row,
        "col": p.col,
    })
}

/// Create the TUI Lua state with the runtime API and default keymaps.
/// What identifies a load error across reloads: where it happened and what
/// it said, with numbers blanked so the same error on another line (after an
/// edit above it) still matches.
fn failure_key((context, message): &(String, String)) -> String {
    let mut key = format!("{context}\n");
    let mut digits = false;
    for c in message.chars() {
        if c.is_ascii_digit() {
            if !digits {
                key.push('#');
            }
            digits = true;
        } else {
            key.push(c);
            digits = false;
        }
    }
    key
}

pub fn init(app: &mut App, config_dir: Option<PathBuf>) {
    let lua = match bone_ui::lua::state::new_state(Side::Tui, config_dir.as_deref()) {
        Ok(lua) => lua,
        Err(e) => {
            if let Some(errors) = &mut app.load_errors {
                errors.push(("lua".to_owned(), short_error(&e)));
            }
            app.error(format!("lua: {}", short_error(&e)));
            return;
        }
    };
    app.lua = Some(lua);
    // Bare loads its own small file in place of the standard UI's.
    let bare = config_dir
        .as_deref()
        .and_then(bone_lua::tui_library)
        .as_deref()
        == Some("bare");
    let files = [
        "ui/api.lua",
        "tui/api.lua",
        if bare {
            "tui/bare.lua"
        } else {
            "tui/defaults.lua"
        },
    ];
    for rel in files {
        let dir = config_dir.clone();
        let r = app.with_api(|lua| {
            let (src, name) = bone_lua::tui_runtime_source(dir.as_deref(), rel)
                .ok_or_else(|| mlua::Error::runtime(format!("missing runtime file {rel}")))?;
            lua.load(src).set_name(format!("@{name}")).exec()
        });
        if let Err(e) = r {
            app.lua_error(&format!("runtime/{rel}"), &e);
        }
    }
}

/// Set the fields of a window that `spec` names (for opening and
/// `bone.ui.update`). Replaced callbacks are released.
fn apply_win_spec(
    app: &mut App,
    lua: &Lua,
    p: &mut crate::app::Popup,
    spec: &Table,
) -> mlua::Result<()> {
    fn err(m: impl std::fmt::Display) -> mlua::Error {
        mlua::Error::runtime(m)
    }
    match spec.get::<Value>("lines")? {
        Value::Nil => {}
        v @ (Value::Function(_) | Value::Table(_)) => {
            if p.lines != 0 {
                App::drop_callback(lua, p.lines)?;
            }
            app.next_callback += 1;
            p.lines = app.next_callback;
            lua.named_registry_value::<Table>(CALLBACKS)?
                .set(p.lines, v)?;
        }
        _ => return Err(err("window lines must be a list or a function")),
    }
    if let Some(t) = spec.get::<Option<Table>>("keys")? {
        for (_, cb) in p.keys.drain(..) {
            App::drop_callback(lua, cb)?;
        }
        for pair in t.pairs::<String, Function>() {
            let (name, f) = pair?;
            let key = keys::parse(&name).map_err(err)?;
            p.keys.push((key, app.store_callback(lua, f)?));
        }
    }
    if let Some(f) = spec.get::<Option<Function>>("on_key")? {
        if let Some(cb) = p.on_key.take() {
            App::drop_callback(lua, cb)?;
        }
        p.on_key = Some(app.store_callback(lua, f)?);
    }
    if let Some(focus) = spec.get::<Option<bool>>("focus")? {
        p.focus = focus;
    }
    if let Some(anchor) = spec.get::<Option<String>>("anchor")? {
        if !matches!(anchor.as_str(), "screen" | "chat" | "prompt") {
            return Err(err(format!(
                "unknown anchor {anchor:?} (screen, chat or prompt)"
            )));
        }
        p.anchor = anchor;
    }
    if let Some(z) = spec.get::<Option<i32>>("z")? {
        p.z = z;
    }
    if let Some(ms) = spec.get::<Option<u64>>("guard")? {
        p.guard = std::time::Duration::from_millis(ms);
    }
    // Size and position: a number sets, false goes back to automatic.
    let num = |k: &str| -> mlua::Result<Option<Option<i64>>> {
        Ok(match spec.get::<Value>(k)? {
            Value::Nil => None,
            Value::Boolean(false) => Some(None),
            Value::Integer(n) => Some(Some(n)),
            Value::Number(n) => Some(Some(n as i64)),
            _ => return Err(err(format!("window {k} must be a number or false"))),
        })
    };
    if let Some(v) = num("width")? {
        p.width = v.map(|n| n.clamp(0, u16::MAX as i64) as u16);
    }
    if let Some(v) = num("height")? {
        p.height = v.map(|n| n.clamp(0, u16::MAX as i64) as u16);
    }
    if let Some(v) = num("row")? {
        p.row = v.map(|n| n as i32);
    }
    if let Some(v) = num("col")? {
        p.col = v.map(|n| n as i32);
    }
    app.dirty = true;
    Ok(())
}

/// The Lua handle of panel `id` (`bone.ui.panel._handle`), passed to its
/// key and close callbacks.
pub fn panel_handle(lua: &Lua, id: &str) -> mlua::Result<Value> {
    let panel: Table = lua
        .globals()
        .get::<Table>("bone")?
        .get::<Table>("ui")?
        .get("panel")?;
    match panel.get::<Option<Function>>("_handle")? {
        Some(f) => f.call(id),
        None => Ok(Value::String(lua.create_string(id)?)),
    }
}

/// A prompt position from Lua: `{ row, col }` (from 0) or a char offset.
fn pos_arg(app: &App, v: Value) -> mlua::Result<crate::editor::Pos> {
    let n = |v: Value, what: &str| -> mlua::Result<usize> {
        match v {
            Value::Integer(n) if n >= 0 => Ok(n as usize),
            Value::Number(n) if n >= 0.0 && n.fract() == 0.0 => Ok(n as usize),
            _ => Err(err(format!("a prompt {what} is a non-negative integer"))),
        }
    };
    match v {
        Value::Table(t) => Ok(app
            .prompt
            .clamp((n(t.get("row")?, "row")?, n(t.get("col")?, "col")?))),
        v @ (Value::Integer(_) | Value::Number(_)) => Ok(app.prompt.position(n(v, "offset")?)),
        other => Err(err(format!(
            "a prompt position is {{ row, col }} or an offset, not {}",
            other.type_name()
        ))),
    }
}

fn pos_out(lua: &Lua, (row, col): crate::editor::Pos) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("row", row)?;
    t.set("col", col)?;
    Ok(t)
}

/// Field `k` of an optional options table: None when it or the table is nil.
/// The Lua handle of job `id` (`bone.job._handle`), passed to its callbacks.
pub fn job_handle(lua: &Lua, id: u64) -> mlua::Result<Value> {
    let job: Table = lua.globals().get::<Table>("bone")?.get("job")?;
    match job.get::<Option<Function>>("_handle")? {
        Some(f) => f.call(id),
        None => Ok(Value::Integer(id as i64)),
    }
}

/// `bone.job.start(cmd, opts)`: the process to run.
fn job_spec(cmd: Value, opts: &Option<Table>) -> mlua::Result<crate::jobs::Spec> {
    use crate::jobs::{Command, Spec, Stdin};
    let command = match cmd {
        Value::String(s) => Command::Shell(s.to_str()?.to_owned()),
        Value::Table(t) => {
            let argv = t
                .sequence_values::<String>()
                .collect::<mlua::Result<Vec<_>>>()?;
            if argv.is_empty() {
                return Err(err("a job's argv list is empty"));
            }
            Command::Argv(argv)
        }
        other => {
            return Err(err(format!(
                "a job is a shell command or an argv list, not {}",
                other.type_name()
            )));
        }
    };
    let get = |k: &str| -> mlua::Result<Value> {
        opts.as_ref().map_or(Ok(Value::Nil), |o| o.get::<Value>(k))
    };
    let stdin = match get("stdin")? {
        Value::Nil | Value::Boolean(false) => Stdin::Null,
        Value::Boolean(true) => Stdin::Open,
        Value::String(s) => Stdin::Text(s.to_str()?.to_owned()),
        _ => return Err(err("job stdin is a string or true")),
    };
    let env = match get("env")? {
        Value::Nil => Vec::new(),
        Value::Table(t) => t
            .pairs::<String, String>()
            .collect::<mlua::Result<Vec<_>>>()?,
        _ => return Err(err("job env is a table of strings")),
    };
    let timeout = match get("timeout")? {
        Value::Nil => None,
        Value::Integer(ms) if ms > 0 => Some(std::time::Duration::from_millis(ms as u64)),
        Value::Number(ms) if ms > 0.0 => Some(std::time::Duration::from_millis(ms as u64)),
        _ => return Err(err("job timeout is a positive number of milliseconds")),
    };
    let pty = match get("pty")? {
        Value::Nil | Value::Boolean(false) => None,
        Value::Boolean(true) => Some((24, 80)),
        Value::Table(t) => {
            screen_size(&Some(t), (24, 80)).map(|(rows, cols, _)| Some((rows, cols)))?
        }
        _ => return Err(err("job pty is true or { width, height }")),
    };
    Ok(Spec {
        command,
        cwd: opt(opts, "cwd")?,
        env,
        stdin,
        timeout,
        pty,
    })
}

/// `{ width, height, scroll }` of a terminal screen: (rows, cols, scroll),
/// the size defaulting to `size`.
fn screen_size(opts: &Option<Table>, size: (u16, u16)) -> mlua::Result<(u16, u16, usize)> {
    let get =
        |k: &str| -> mlua::Result<Option<i64>> { opts.as_ref().map_or(Ok(None), |t| t.get(k)) };
    Ok((
        get("height")?.map_or(size.0, |h| h.clamp(1, 500) as u16),
        get("width")?.map_or(size.1, |w| w.clamp(1, 1000) as u16),
        get("scroll")?.unwrap_or(0).max(0) as usize,
    ))
}

/// What `bone.processes.screen` and `job:screen` give: the screen's lines as
/// `{ { text, group }, … }` rows, and how far it is scrolled back.
fn screen_table(
    lua: &Lua,
    lines: &[Vec<crate::ui::Item>],
    scroll: usize,
    max: usize,
) -> mlua::Result<Table> {
    let out = lua.create_table()?;
    for (i, line) in lines.iter().enumerate() {
        let l = lua.create_table()?;
        for (j, item) in line.iter().enumerate() {
            if let crate::ui::Item::Text(text, group) = item {
                l.set(
                    j + 1,
                    lua.create_sequence_from([text.as_str(), group.as_str()])?,
                )?;
            }
        }
        out.set(i + 1, l)?;
    }
    let r = lua.create_table()?;
    r.set("lines", out)?;
    r.set("scroll", scroll)?;
    r.set("max", max)?;
    Ok(r)
}

fn valid_panel_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// Set the fields of a panel that `spec` names (for opening and
/// `update`). Replaced callbacks are released. `focus` is handled by the
/// caller once the panel is in place.
fn apply_panel_spec(
    app: &mut App,
    lua: &Lua,
    p: &mut crate::panel::Panel,
    spec: &Table,
) -> mlua::Result<()> {
    use crate::panel::{Dock, Size};
    let content = match (spec.get::<Value>("render")?, spec.get::<Value>("lines")?) {
        (Value::Nil, Value::Nil) => None,
        (f @ Value::Function(_), Value::Nil) => Some(f),
        (Value::Nil, v @ (Value::Function(_) | Value::Table(_))) => Some(v),
        (Value::Nil, _) => return Err(err("panel lines must be a list or a function")),
        (_, Value::Nil) => return Err(err("panel render must be a function")),
        _ => return Err(err("give a panel render or lines, not both")),
    };
    if let Some(v) = content {
        if p.content != 0 {
            App::drop_callback(lua, p.content)?;
        }
        app.next_callback += 1;
        p.content = app.next_callback;
        lua.named_registry_value::<Table>(CALLBACKS)?
            .set(p.content, v)?;
        app.ui_broken.remove(&format!("panel.{}", p.id));
    }
    if let Some(dock) = spec.get::<Option<String>>("dock")? {
        p.dock = Dock::parse(&dock).ok_or_else(|| {
            err(format!(
                "unknown dock {dock:?} (left, right, top or bottom)"
            ))
        })?;
    }
    if let Some(full_height) = spec.get::<Option<bool>>("full_height")? {
        p.full_height = full_height;
    }
    if let Some(separator) = spec.get::<Option<bool>>("separator")? {
        p.separator = separator;
    }
    match spec.get::<Value>("size")? {
        Value::Nil => {}
        Value::Boolean(false) => p.size = None,
        Value::String(s) if s.to_str()?.as_ref() == "auto" => p.size = Some(Size::Auto),
        Value::Integer(n) if n >= 1 => p.size = Some(Size::Cells(n.min(u16::MAX as i64) as u16)),
        Value::Number(f) if f > 0.0 && f < 1.0 => p.size = Some(Size::Fraction(f)),
        Value::Number(f) if f >= 1.0 => p.size = Some(Size::Cells(f.min(u16::MAX as f64) as u16)),
        _ => {
            return Err(err(
                "panel size is a number of cells, a fraction between 0 and 1, \"auto\" or false",
            ));
        }
    }
    match spec.get::<Value>("max")? {
        Value::Nil => {}
        Value::Boolean(false) => p.max = None,
        Value::Integer(n) if n >= 1 => p.max = Some(n.min(u16::MAX as i64) as u16),
        _ => return Err(err("panel max must be a positive integer or false")),
    }
    if let Some(min) = spec.get::<Option<u16>>("min")? {
        p.min = min;
    }
    if let Some(order) = spec.get::<Option<i32>>("order")? {
        p.order = order;
    }
    match spec.get::<Value>("title")? {
        Value::Nil => {}
        Value::Boolean(false) => p.title = None,
        Value::String(t) => p.title = Some(crate::text::sanitize(&t.to_str()?).replace('\n', " ")),
        _ => return Err(err("panel title must be a string or false")),
    }
    if let Some(t) = spec.get::<Option<Table>>("keys")? {
        let mut keys = Vec::new();
        for pair in t.pairs::<String, Function>() {
            let (name, f) = pair?;
            keys.push((keys::parse(&name).map_err(err)?, f));
        }
        for (_, cb) in p.keys.drain(..) {
            App::drop_callback(lua, cb)?;
        }
        for (key, f) in keys {
            p.keys.push((key, app.store_callback(lua, f)?));
        }
    }
    for (field, slot) in [("on_key", &mut p.on_key), ("on_close", &mut p.on_close)] {
        if let Some(f) = spec.get::<Option<Function>>(field)? {
            if let Some(cb) = slot.take() {
                App::drop_callback(lua, cb)?;
            }
            *slot = Some(app.store_callback(lua, f)?);
        }
    }
    match spec.get::<Value>("context")? {
        Value::Nil => {}
        Value::Boolean(false) => p.context = None,
        Value::String(name) => {
            let ctx = context_from_name(name.to_str()?.to_owned())?;
            if matches!(ctx, Context::Main | Context::Popup) {
                return Err(err(format!(
                    "a panel's context cannot be {} (use panel or a named context)",
                    ctx.name()
                )));
            }
            p.context = Some(ctx);
        }
        _ => return Err(err("panel context must be a name or false")),
    }
    if let Some(v) = spec.get::<Option<bool>>("focusable")? {
        p.focusable = v;
    }
    if let Some(v) = spec.get::<Option<bool>>("hidden")? {
        p.hidden = v;
    }
    if let Some(v) = spec.get::<Option<bool>>("follow")? {
        p.follow = v;
        p.at_end = v;
    }
    app.dirty = true;
    Ok(())
}

/// Close panel `id`: release its callbacks, then run `on_close(panel)` and
/// the `panel/closed` event. Returns whether it was open.
fn close_panel(app: &mut App, lua: &Lua, id: &str) -> mlua::Result<bool> {
    let Some(i) = app.panels.iter().position(|p| p.id == id) else {
        return Ok(false);
    };
    let event = app.panel_info(&app.panels[i]);
    let p = app.panels.remove(i);
    App::drop_callback(lua, p.content)?;
    for (_, cb) in p.keys {
        App::drop_callback(lua, cb)?;
    }
    if let Some(cb) = p.on_key {
        App::drop_callback(lua, cb)?;
    }
    app.check_panel_focus();
    if let Some(cb) = p.on_close {
        let id = id.to_owned();
        app.call_callback(cb, "panel on_close", |lua| panel_handle(lua, &id));
        App::drop_callback(lua, cb)?;
    }
    app.fire("panel/closed", event);
    app.dirty = true;
    Ok(true)
}

impl App {
    /// Run Lua with the API bound to this app.
    pub fn with_api<R>(&mut self, body: impl FnOnce(&Lua) -> mlua::Result<R>) -> mlua::Result<R> {
        let Some(lua) = self.lua.clone() else {
            return Err(mlua::Error::runtime("Lua is not available"));
        };
        bone_ui::lua::state::lend(self, &lua, dispatch, body)
    }

    pub fn lua_error(&mut self, context: &str, e: &mlua::Error) {
        if let Some(errors) = &mut self.load_errors {
            errors.push((context.to_owned(), short_error(e)));
        }
        self.log.push(format!("{context}: {e}"));
        // The last few in full, for /health.
        self.lua_errors.push(format!("{context}: {e}"));
        if self.lua_errors.len() > 3 {
            self.lua_errors.remove(0);
        }
        self.error(format!("{context}: {}", short_error(e)));
    }

    /// Load each plugin's `ui.lua` and `tui.lua` (as that plugin), then
    /// `<config dir>/ui.lua` and `tui.lua`, then a trusted project's, then
    /// fire `ready`. Where this had errors is remembered for reloads.
    pub fn load_user_config(&mut self) {
        self.load_user_files();
        self.fire("ready", serde_json::Value::Null);
        self.load_failures = self
            .load_errors
            .take()
            .unwrap_or_default()
            .iter()
            .map(failure_key)
            .collect();
    }

    fn load_user_files(&mut self) {
        if let Some(dir) = self.config_dir.clone() {
            // Plugins switched off in settings.json are listed, not loaded.
            let disabled: Vec<String> = self
                .settings
                .get("plugins")
                .and_then(|p| p.get("disabled"))
                .and_then(|d| d.as_array())
                .map(|l| {
                    l.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            for plugin in bone_lua::plugins(&dir) {
                let name = bone_lua::plugin_name(&plugin);
                if disabled.contains(&name) {
                    self.register_plugin(&name, &plugin, crate::plugins::Kind::Plugin);
                } else {
                    self.load_plugin(&name, &plugin, crate::plugins::Kind::Plugin);
                }
            }
            // Saved settings, after the defaults and plugins (which define
            // the options) and before tui.lua, which runs last.
            self.apply_tui_settings(None);
            // Bare is the plain UI: tui.lua and the project's would build
            // a full one on top of it.
            if bone_lua::tui_library(&dir).as_deref() == Some("bare") {
                return;
            }
            for path in CONFIG_FILES.map(|f| dir.join(f)) {
                if let Err(e) = self.with_api(|lua| bone_lua::run_file(lua, &path)) {
                    self.lua_error(&path.display().to_string(), &e);
                }
            }
            self.load_project();
        }
    }

    /// Apply saved TUI options, skipping values unchanged from `previous`.
    /// Unknown or invalid options are reported, as with `bone.o.<name> = value`.
    pub fn apply_tui_settings(&mut self, previous: Option<&Json>) {
        let Some(tui) = self
            .settings
            .get("tui")
            .and_then(|t| t.as_object())
            .cloned()
        else {
            return;
        };
        // Bare defines none of the standard options these may be saved for.
        let bare = self
            .config_dir
            .as_deref()
            .and_then(bone_lua::tui_library)
            .as_deref()
            == Some("bare");
        for (name, value) in tui {
            if previous.is_some_and(|old| value.is_null() || old.get(&name) == Some(&value)) {
                continue;
            }
            let r = self.with_api(|lua| {
                let o: Table = lua.globals().get::<Table>("bone")?.get("o")?;
                o.set(name.as_str(), to_lua(lua, &value)?)
            });
            if let Err(e) = r {
                if bare && short_error(&e).contains("unknown option") {
                    continue;
                }
                self.lua_error(&format!("settings.json tui.{name}"), &e);
            }
        }
    }

    pub fn update_settings(&mut self, settings: Json) {
        let previous = std::mem::replace(&mut self.settings, settings);
        self.apply_tui_settings(Some(&previous["tui"]));
    }

    /// Load the Lua configuration again after a file changed, the way
    /// startup does (API, defaults, plugins, `tui.lua`, then `ready` with
    /// `{ reload = true, panels = { ids open before } }`), into a fresh state
    /// while the current one waits. The new one is taken only if it loaded
    /// without a Lua error the current one did not also have; otherwise the
    /// current one and the built-in options stay as they were and the error
    /// is shown. On success the old state's shutdown hooks run and it is
    /// dropped. The session, prompt, chats and options' values are Rust's
    /// and stay.
    pub fn reload_user_config(&mut self) -> bool {
        // The new state reads plugin state from disk, so write it first.
        self.save_plugin_states(None);
        self.carried_options = self
            .dynamic_options
            .iter()
            .map(|(name, o)| (name.clone(), o.value.clone()))
            .collect();
        let panels: Vec<String> = self.panels.iter().map(|p| p.id.clone()).collect();
        let options = self.options.clone();
        let old = self.swap_lua_side(Default::default());
        self.load_errors = Some(Vec::new());
        let dir = self.config_dir.clone();
        init(self, dir);
        self.load_user_files();
        self.carried_options.clear();
        // Its handlers are part of the new config: an error there counts too.
        self.fire(
            "ready",
            serde_json::json!({ "reload": true, "panels": panels }),
        );
        let errors = self.load_errors.take().unwrap_or_default();
        let fresh = errors
            .iter()
            .find(|e| !self.load_failures.contains(&failure_key(e)))
            .cloned();
        if self.lua.is_none() || fresh.is_some() {
            let new = self.swap_lua_side(old);
            self.retire_lua_side(new);
            self.options = options;
            let (context, message) = fresh.unwrap_or_else(|| ("lua".into(), "cannot start".into()));
            // The restored state's list, for /health.
            self.lua_errors
                .push(format!("reload: {context}: {message}"));
            if self.lua_errors.len() > 3 {
                self.lua_errors.remove(0);
            }
            self.error(format!(
                "Lua reload failed, kept the previous configuration. {context}: {message}"
            ));
            return false;
        }
        self.load_failures = errors.iter().map(failure_key).collect();
        // The old state's shutdown hooks run on it, just before it goes.
        let new = self.swap_lua_side(old);
        self.run_shutdown(None);
        let old = self.swap_lua_side(new);
        self.retire_lua_side(old);
        self.info("Lua configuration reloaded");
        true
    }

    /// Keep `f` for later; it runs as the plugin that stored it.
    fn store_callback(&mut self, lua: &Lua, f: Function) -> mlua::Result<u64> {
        bone_ui::lua::store_callback(self, lua, f)
    }

    /// Run `f` as the plugin that owns callback `cb` (or as the user's
    /// config), so what it creates belongs to that plugin.
    pub fn as_owner_of<R>(&mut self, cb: u64, f: impl FnOnce(&mut App) -> R) -> R {
        <App as bone_ui::lua::Host>::as_owner_of(self, cb, f)
    }

    /// Forget stored callbacks (outside a Lua call).
    pub fn release_callbacks(&mut self, ids: &[u64]) {
        if ids.is_empty() {
            return;
        }
        let _ = self.with_api(|lua| {
            for id in ids {
                App::drop_callback(lua, *id)?;
            }
            Ok(())
        });
    }

    fn drop_callback(lua: &Lua, id: u64) -> mlua::Result<()> {
        bone_ui::lua::drop_callback(lua, id)
    }

    /// Call a stored callback with one argument built in Lua.
    pub fn call_callback(
        &mut self,
        id: u64,
        context: &str,
        arg: impl FnOnce(&Lua) -> mlua::Result<Value>,
    ) -> Option<Value> {
        let r = self.as_owner_of(id, |app| {
            app.with_api(|lua| {
                let f: Function = lua.named_registry_value::<Table>(CALLBACKS)?.get(id)?;
                f.call::<Value>(arg(lua)?)
            })
        });
        match r {
            Ok(v) => Some(v),
            Err(e) => {
                self.lua_error(context, &e);
                None
            }
        }
    }

    pub fn call_callback_multi(
        &mut self,
        id: u64,
        context: &str,
        args: impl FnOnce(&Lua) -> mlua::Result<MultiValue>,
    ) -> Option<MultiValue> {
        let r = self.as_owner_of(id, |app| {
            app.with_api(|lua| {
                let f: Function = lua.named_registry_value::<Table>(CALLBACKS)?.get(id)?;
                f.call::<MultiValue>(args(lua)?)
            })
        });
        match r {
            Ok(v) => Some(v),
            Err(e) => {
                self.lua_error(context, &e);
                None
            }
        }
    }

    /// Run `event` autocommands with `data`. Returns their results in order.
    pub fn fire(&mut self, event: &str, data: serde_json::Value) -> Vec<Value> {
        bone_ui::lua::fire(self, event, data)
    }

    pub fn has_autocmd(&self, event: &str) -> bool {
        let event = canonical_event(event);
        self.autocmds.iter().any(|a| a.event == event)
    }

    /// Run Lua (`bone.api.exec_lua`): `code`, or `=expr` to show a value.
    pub fn exec_lua(&mut self, code: &str) {
        let (code, show) = match code.strip_prefix('=') {
            Some(expr) => (format!("return {expr}"), true),
            None => (code.to_owned(), false),
        };
        let r = self.with_api(|lua| {
            let v: Value = lua.load(code).set_name("=/lua").eval()?;
            if !show {
                return Ok(None);
            }
            let inspect: Function = lua.globals().get::<Table>("bone")?.get("inspect")?;
            Ok(Some(inspect.call::<String>(v)?))
        });
        match r {
            Ok(Some(s)) => self.info(s),
            Ok(None) => {}
            Err(e) => self.lua_error("lua", &e),
        }
    }

    pub fn source(&mut self, path: &str) {
        let path = PathBuf::from(path);
        match self.with_api(|lua| bone_lua::run_file(lua, &path)) {
            Ok(true) => {}
            Ok(false) => self.error(format!("no such file: {}", path.display())),
            Err(e) => self.lua_error(&path.display().to_string(), &e),
        }
    }
}

fn raw_context_arg(opts: &Option<Table>) -> mlua::Result<Option<Context>> {
    opt(opts, "context")?.map(context_from_name).transpose()
}

fn dispatch(app: &mut App, lua: &Lua, op: &str, a: MultiValue) -> mlua::Result<MultiValue> {
    let a = match bone_ui::lua::dispatch(app, lua, op, a) {
        Ok(r) => return r,
        Err(a) => a,
    };
    match op {
        "keymap_focus" => {
            let name: Option<String> = args(lua, a)?;
            match name {
                Some(name) => app.focus_context(context_from_name(name)?).map_err(err)?,
                None => app.clear_context().map_err(err)?,
            }
            ret(lua, ())
        }
        "keymap_clear" => {
            let _: () = args(lua, a)?;
            app.clear_context().map_err(err)?;
            ret(lua, ())
        }
        "keymap_context_del" => {
            let name: String = args(lua, a)?;
            let ctx = context_from_name(name)?;
            app.delete_context(&ctx).map_err(err)?;
            ret(lua, ())
        }
        "keymap_raw" => {
            let (f, opts): (Function, Option<Table>) = args(lua, a)?;
            let context = raw_context_arg(&opts)?;
            let callback = app.store_callback(lua, f)?;
            let id = app.add_raw_interceptor(callback, context);
            app.own(crate::plugins::Owned::Raw(id));
            ret(lua, id)
        }
        "keymap_raw_del" => {
            let id: u64 = args(lua, a)?;
            match app.remove_raw_interceptor(id) {
                Some(callback) => {
                    App::drop_callback(lua, callback)?;
                    ret(lua, true)
                }
                None => ret(lua, false),
            }
        }
        "press" => {
            let name: String = args(lua, a)?;
            app.handle_key(keys::parse(&name).map_err(err)?);
            ret(lua, ())
        }
        "action" => {
            let name: String = args(lua, a)?;
            let b =
                Builtin::from_name(&name).ok_or_else(|| err(format!("unknown action: {name}")))?;
            app.run(Action::Builtin(b));
            ret(lua, ())
        }
        "clipboard_paste" => {
            app.paste_clipboard();
            ret(lua, ())
        }
        "clipboard_copy" => {
            // Copied after the next draw, like a mouse selection.
            (app.clipboard, app.dirty) = (Some(args(lua, a)?), true);
            ret(lua, ())
        }
        "image_attach" => {
            let path: String = args(lua, a)?;
            app.attach_file(path.into());
            ret(lua, ())
        }
        "image_remove" => {
            let index: usize = args(lua, a)?;
            app.remove_image(index).map_err(err)?;
            ret(lua, ())
        }
        "prompt_empty" => ret(lua, app.draft_empty()),
        "prompt_set" => {
            let text: String = args(lua, a)?;
            app.set_prompt_text(&text);
            ret(lua, ())
        }
        "health" => ret(
            lua,
            to_lua(lua, &serde_json::Value::Array(app.tui_health()))?,
        ),
        "open_session" => {
            // No session: a new chat (for a window to show).
            let (id, show): (Option<String>, Option<bool>) = args(lua, a)?;
            let buf = match id {
                Some(id) => app.open_session(id, show.unwrap_or(true)),
                None => {
                    app.chats.push(crate::chat::ChatBuffer::new(None));
                    app.chats.len() - 1
                }
            };
            ret(lua, buf)
        }
        "geometry" => {
            let rect = |r: &ratatui::layout::Rect| {
                serde_json::json!({
                    "x": r.x, "y": r.y, "width": r.width, "height": r.height,
                })
            };
            let leaves: Vec<_> = (app.leaves.iter())
                .filter(|(_, r)| r.width > 0 && r.height > 0)
                .map(|(name, r)| {
                    let mut v = rect(r);
                    v["name"] = name.clone().into();
                    v
                })
                .collect();
            let seps: Vec<_> = (app.seps.iter())
                .map(|(r, path)| {
                    let mut v = rect(r);
                    v["path"] = path.clone().into();
                    v
                })
                .collect();
            let g = serde_json::json!({ "leaves": leaves, "seps": seps });
            ret(lua, to_lua(lua, &g)?)
        }
        "popup_open" => {
            let spec: Table = args(lua, a)?;
            let old_focus = app.focused_popup().map(|p| p.id);
            app.next_callback += 1;
            let mut p = crate::app::Popup {
                id: app.next_callback,
                focus: false,
                anchor: "screen".into(),
                z: 0,
                lines: 0,
                keys: Vec::new(),
                on_key: None,
                width: None,
                height: None,
                row: None,
                col: None,
                rect: None,
                opened: std::time::Instant::now(),
                guard: std::time::Duration::ZERO,
            };
            if !matches!(
                spec.get::<Value>("lines")?,
                Value::Function(_) | Value::Table(_)
            ) {
                return Err(err("a window needs lines: a list or a function"));
            }
            apply_win_spec(app, lua, &mut p, &spec)?;
            let id = p.id;
            let event = popup_event(&p);
            app.own(crate::plugins::Owned::Popup(id));
            app.popups.push(p);
            app.fire("panel/opened", event);
            if old_focus != app.focused_popup().map(|p| p.id) {
                app.emit_focus_changed();
            }
            app.dirty = true;
            ret(lua, id)
        }
        "popup_update" => {
            let (id, spec): (u64, Table) = args(lua, a)?;
            let Some(i) = app.popups.iter().position(|p| p.id == id) else {
                return ret(lua, false);
            };
            let old_focus = app.focused_popup().map(|p| p.id);
            let mut p = app.popups.remove(i);
            p.rect = None;
            if let Err(error) = apply_win_spec(app, lua, &mut p, &spec) {
                app.popups.insert(i, p);
                return Err(error);
            }
            let event = popup_event(&p);
            app.popups.insert(i, p);
            app.fire("panel/updated", event);
            if old_focus != app.focused_popup().map(|p| p.id) {
                app.emit_focus_changed();
            }
            app.dirty = true;
            ret(lua, true)
        }
        "popup_is_open" => {
            let id: u64 = args(lua, a)?;
            ret(lua, app.popups.iter().any(|p| p.id == id))
        }
        "popup_close" => {
            let id: u64 = args(lua, a)?;
            if let Some(i) = app.popups.iter().position(|p| p.id == id) {
                let old_focus = app.focused_popup().map(|p| p.id);
                let p = app.popups.remove(i);
                let event = popup_event(&p);
                App::drop_callback(lua, p.lines)?;
                if let Some(cb) = p.on_key {
                    App::drop_callback(lua, cb)?;
                }
                for (_, cb) in p.keys {
                    App::drop_callback(lua, cb)?;
                }
                app.fire("panel/closed", event);
                if old_focus != app.focused_popup().map(|p| p.id) {
                    app.emit_focus_changed();
                }
                app.dirty = true;
            }
            ret(lua, ())
        }
        "job_start" => {
            let (cmd, opts): (Value, Option<Table>) = args(lua, a)?;
            let spec = job_spec(cmd, &opts)?;
            let mut stored = Vec::new();
            let mut callback = |app: &mut App, k: &str| -> mlua::Result<Option<u64>> {
                match opt(&opts, k)? {
                    None => Ok(None),
                    Some(Value::Function(f)) => {
                        let id = app.store_callback(lua, f)?;
                        stored.push(id);
                        Ok(Some(id))
                    }
                    Some(_) => Err(err(format!("job {k} must be a function"))),
                }
            };
            let cbs = (|| {
                Ok::<_, mlua::Error>((
                    callback(app, "on_stdout")?,
                    callback(app, "on_stderr")?,
                    callback(app, "on_exit")?,
                ))
            })();
            let (on_stdout, on_stderr, on_exit) = match cbs {
                Ok(c) => c,
                Err(e) => {
                    for id in stored {
                        App::drop_callback(lua, id)?;
                    }
                    return Err(e);
                }
            };
            let flag = |k: &str| -> mlua::Result<Option<bool>> {
                opts.as_ref().map_or(Ok(None), |o| o.get(k))
            };
            let id = app.start_job(
                spec,
                crate::jobs::Callbacks {
                    name: opt(&opts, "name")?,
                    lines: flag("lines")?.unwrap_or(false),
                    // Without output callbacks, keep the output for on_exit.
                    buffer: flag("buffer")?.unwrap_or(on_stdout.is_none() && on_stderr.is_none()),
                    on_stdout,
                    on_stderr,
                    on_exit,
                },
            );
            app.own(crate::plugins::Owned::Job(id));
            ret(lua, id)
        }
        "job_cancel" => {
            let id: u64 = args(lua, a)?;
            ret(lua, app.cancel_job(id))
        }
        "job_write" => {
            let (id, data): (u64, mlua::String) = args(lua, a)?;
            ret(lua, app.write_job(id, data.as_bytes().to_vec()))
        }
        "job_close_stdin" => {
            let id: u64 = args(lua, a)?;
            ret(lua, app.close_job_stdin(id))
        }
        "job_status" => {
            let id: u64 = args(lua, a)?;
            match app.jobs.get(id) {
                Some(j) => ret(lua, to_lua(lua, &j.status())?),
                None => ret(lua, Value::Nil),
            }
        }
        "job_list" => {
            let list: Vec<serde_json::Value> = app.jobs.list.iter().map(|j| j.status()).collect();
            ret(lua, to_lua(lua, &serde_json::Value::Array(list))?)
        }
        "process_screen" => {
            let (id, opts): (String, Option<mlua::Table>) = args(lua, a)?;
            let (rows, cols, scroll) = screen_size(&opts, (24, 80))?;
            let (lines, scroll, max) = app.process_screen(&id, rows, cols, scroll);
            ret(lua, screen_table(lua, &lines, scroll, max)?)
        }
        "job_screen" => {
            let (id, opts): (u64, Option<mlua::Table>) = args(lua, a)?;
            // Without a size it stays the size it is.
            let size = app
                .jobs
                .get(id)
                .and_then(|j| Some(j.term.as_ref()?.state()?.size()));
            let (rows, cols, scroll) = screen_size(&opts, size.unwrap_or((24, 80)))?;
            let Some((lines, scroll, max, state)) = app.job_screen(id, rows, cols, scroll) else {
                return ret(lua, Value::Nil);
            };
            let r = screen_table(lua, &lines, scroll, max)?;
            if let Value::Table(state) = to_lua(lua, &state)? {
                for kv in state.pairs::<Value, Value>() {
                    let (k, v) = kv?;
                    r.set(k, v)?;
                }
            }
            ret(lua, r)
        }
        "process_refresh" => {
            if let Some(session_id) = app.current_session_id() {
                app.request_processes(session_id);
            }
            ret(lua, ())
        }
        "process_cancel" => {
            let id: String = args(lua, a)?;
            let Some(session_id) = app.current_session_id() else {
                return ret(lua, false);
            };
            app.request::<ProcessCancel>(ProcessRef { session_id, id }, |app, result| {
                if let Err(e) = result {
                    app.error(format!("cannot cancel shell process: {e}"));
                }
            });
            ret(lua, true)
        }
        "plugin_current" => {
            let Some(name) = app.owner.clone() else {
                return ret(lua, Value::Nil);
            };
            let info = app
                .plugin(&name)
                .map(|p| p.info())
                .unwrap_or_else(|| serde_json::json!({ "name": name }));
            ret(lua, to_lua(lua, &info)?)
        }
        "plugin_list" => {
            let list: Vec<serde_json::Value> = app.plugins.iter().map(|p| p.info()).collect();
            ret(lua, to_lua(lua, &serde_json::Value::Array(list))?)
        }
        "plugin_load" | "plugin_unload" | "plugin_reload" => {
            let name: String = args(lua, a)?;
            if app.owner.as_deref() == Some(name.as_str()) && op != "plugin_load" {
                return Err(err(format!(
                    "plugin {name} cannot unload itself while it runs"
                )));
            }
            app.rewatch = true;
            match op {
                "plugin_load" => app.load_plugin_by_name(&name),
                "plugin_unload" => app.unload_plugin(&name),
                _ => app.reload_plugin(&name),
            }
            .map_err(err)?;
            ret(lua, ())
        }
        "plugin_on_shutdown" => {
            let f: Function = args(lua, a)?;
            let cb = app.store_callback(lua, f)?;
            app.shutdown_hooks.push((app.owner.clone(), cb));
            ret(lua, ())
        }
        // The command registry, for the Lua `/` menu and commands.
        "history_add" => {
            let text: String = args(lua, a)?;
            app.history_add(&text);
            ret(lua, ())
        }
        "log_tail" => {
            let n: usize = args(lua, a)?;
            let start = app.log.len().saturating_sub(n);
            ret(lua, app.log[start..].to_vec())
        }
        "show_message" => {
            // Shown without adding to the log (for showing the log itself).
            let text: String = args(lua, a)?;
            app.message = (!text.is_empty()).then_some((text, crate::app::Level::Info));
            app.dirty = true;
            ret(lua, ())
        }
        "exec_lua" => {
            let code: String = args(lua, a)?;
            app.exec_lua(&code);
            ret(lua, ())
        }
        "source_file" => {
            let path: String = args(lua, a)?;
            app.source(&path);
            ret(lua, ())
        }
        "project_trust" => {
            let on: bool = args(lua, a)?;
            app.set_project_trust(on).map_err(err)?;
            ret(lua, ())
        }
        "project_info" => match app.project_info() {
            Some(info) => ret(lua, to_lua(lua, &info)?),
            None => ret(lua, Value::Nil),
        },
        "panel_open" => {
            let spec: Table = args(lua, a)?;
            app.next_callback += 1;
            let seq = app.next_callback;
            let id = match spec.get::<Option<String>>("id")? {
                Some(id) if !valid_panel_id(&id) => {
                    return Err(err(format!(
                        "invalid panel id {id:?} (letters, digits, _, - and .)"
                    )));
                }
                Some(id) if app.panel(&id).is_some() => {
                    return Err(err(format!("panel {id} is already open")));
                }
                Some(id) => id,
                None => {
                    let mut n = seq;
                    while app.panel(&format!("panel{n}")).is_some() {
                        n += 1;
                    }
                    format!("panel{n}")
                }
            };
            if matches!(
                (spec.get::<Value>("render")?, spec.get::<Value>("lines")?),
                (Value::Nil, Value::Nil)
            ) {
                return Err(err("a panel needs render (a function) or lines"));
            }
            let focus = spec.get::<Option<bool>>("focus")? == Some(true);
            let mut p = crate::panel::Panel::new(id.clone(), seq);
            let applied = apply_panel_spec(app, lua, &mut p, &spec).and_then(|()| {
                if focus && (p.hidden || !p.focusable) {
                    return Err(err("a hidden or unfocusable panel cannot open focused"));
                }
                Ok(())
            });
            if let Err(e) = applied {
                // Release whatever was stored before the bad field.
                for cb in [p.content]
                    .into_iter()
                    .chain(p.keys.iter().map(|(_, cb)| *cb))
                    .chain(p.on_key)
                    .chain(p.on_close)
                    .filter(|cb| *cb != 0)
                {
                    App::drop_callback(lua, cb)?;
                }
                return Err(e);
            }
            let event = app.panel_info(&p);
            app.own(crate::plugins::Owned::Panel(id.clone(), seq));
            app.panels.push(p);
            app.fire("panel/opened", event);
            if focus {
                // An opened callback may have closed or hidden it again.
                let _ = app.focus_panel(Some(&id));
            }
            ret(lua, id)
        }
        "panel_update" => {
            let (id, spec): (String, Table) = args(lua, a)?;
            let Some(i) = app.panels.iter().position(|p| p.id == id) else {
                return ret(lua, false);
            };
            let mut p = app.panels.remove(i);
            let applied = apply_panel_spec(app, lua, &mut p, &spec);
            app.panels.insert(i, p);
            applied?;
            app.check_panel_focus();
            match spec.get::<Option<bool>>("focus")? {
                Some(true) => app.focus_panel(Some(&id)).map_err(err)?,
                Some(false) if app.focused_panel().is_some_and(|p| p.id == id) => {
                    app.focus_panel(None).map_err(err)?
                }
                _ => {}
            }
            if let Some(p) = app.panel(&id) {
                let event = app.panel_info(p);
                app.fire("panel/updated", event);
            }
            ret(lua, true)
        }
        "panel_set_lines" => {
            let (id, lines): (String, Value) = args(lua, a)?;
            if !matches!(lines, Value::Table(_) | Value::Function(_)) {
                return Err(err("panel lines must be a list or a function"));
            }
            let Some(content) = app.panel(&id).map(|p| p.content) else {
                return ret(lua, false);
            };
            lua.named_registry_value::<Table>(CALLBACKS)?
                .set(content, lines)?;
            app.ui_broken.remove(&format!("panel.{id}"));
            app.dirty = true;
            ret(lua, true)
        }
        "panel_close" => {
            let id: String = args(lua, a)?;
            ret(lua, close_panel(app, lua, &id)?)
        }
        "panel_info" => {
            let id: String = args(lua, a)?;
            match app.panel(&id) {
                Some(p) => ret(lua, to_lua(lua, &app.panel_info(p))?),
                None => ret(lua, Value::Nil),
            }
        }
        "panel_list" => {
            let list: Vec<serde_json::Value> = app
                .panels_in_order()
                .into_iter()
                .map(|p| app.panel_info(p))
                .collect();
            ret(lua, to_lua(lua, &serde_json::Value::Array(list))?)
        }
        "panel_focus" => {
            let id: Option<String> = args(lua, a)?;
            app.focus_panel(id.as_deref()).map_err(err)?;
            ret(lua, ())
        }
        "panel_scroll" => {
            let (id, to): (String, Value) = args(lua, a)?;
            let Some(p) = app.panel_mut(&id) else {
                return ret(lua, false);
            };
            match to {
                Value::Integer(n) => p.scroll_by(n),
                Value::Number(n) => p.scroll_by(n as i64),
                Value::String(s) if s.to_str()?.as_ref() == "top" => p.scroll_to(false),
                Value::String(s) if s.to_str()?.as_ref() == "bottom" => p.scroll_to(true),
                _ => {
                    return Err(err(
                        "scroll by a number of rows, or to \"top\" or \"bottom\"",
                    ));
                }
            }
            app.dirty = true;
            ret(lua, true)
        }
        "ui_refresh_in" => {
            let ms: f64 = args(lua, a)?;
            let at =
                std::time::Instant::now() + std::time::Duration::from_millis(ms.max(0.0) as u64);
            app.ui_expiry = Some(app.ui_expiry.map_or(at, |e| e.min(at)));
            ret(lua, ())
        }
        "runtime_overrides" => {
            let list: Vec<serde_json::Value> = app
                .config_dir
                .as_deref()
                .map(bone_lua::runtime_overrides)
                .unwrap_or_default()
                .into_iter()
                .map(|(path, same)| serde_json::json!({ "path": path, "same": same }))
                .collect();
            ret(lua, to_lua(lua, &serde_json::Value::Array(list))?)
        }
        "runtime_reset" => {
            let (rel, stamp): (String, Option<String>) = args(lua, a)?;
            let dir = app
                .config_dir
                .clone()
                .ok_or_else(|| err("there is no config directory"))?;
            // One backup folder per reset (or per `/runtime reset all`).
            let stamp = stamp.unwrap_or_else(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs())
                    .to_string()
            });
            let backup = dir.join("runtime-backup").join(&stamp);
            let to = bone_lua::reset_runtime_file(&dir, &rel, &backup).map_err(err)?;
            ret(lua, to.to_string_lossy().into_owned())
        }
        "library_info" => {
            let dir = app.config_dir.as_deref();
            let info = serde_json::json!({
                "active": dir.and_then(bone_lua::tui_library).unwrap_or_else(|| "standard".into()),
                "custom": dir.map(bone_lua::tui_libraries).unwrap_or_default(),
                "dir": dir.map(|d| d.join("libraries").to_string_lossy().into_owned()),
            });
            ret(lua, to_lua(lua, &info)?)
        }
        "library_create" | "library_delete" => {
            let (name, from): (String, Option<String>) = args(lua, a)?;
            let dir = app
                .config_dir
                .clone()
                .ok_or_else(|| err("there is no config directory"))?;
            let path = if op == "library_create" {
                bone_lua::create_tui_library(&dir, &name, from.as_deref() == Some("bare"))
            } else {
                bone_lua::delete_tui_library(&dir, &name)
            };
            ret(lua, path.map_err(err)?.to_string_lossy().into_owned())
        }
        "library_use" => {
            let name: String = args(lua, a)?;
            let dir = app
                .config_dir
                .clone()
                .ok_or_else(|| err("there is no config directory"))?;
            let prev = bone_lua::tui_library(&dir).unwrap_or_else(|| "standard".into());
            bone_lua::set_tui_library(&dir, &name).map_err(err)?;
            app.library_prev = Some(prev);
            // Not from inside this callback: the reload replaces its state.
            app.reload_requested = true;
            ret(lua, ())
        }
        "chat_at" => {
            let (x, y): (u16, u16) = args(lua, a)?;
            match app.chat_at((x, y)) {
                Some(hit) => ret(lua, to_lua(lua, &hit)?),
                None => ret(lua, Value::Nil),
            }
        }
        "chat_scroll_to" => {
            let (index, at): (usize, Option<String>) = args(lua, a)?;
            let ok = app.chat_scroll_to(index, at.as_deref().unwrap_or("top"));
            ret(lua, ok)
        }
        "chat_scroll" => {
            let by: Value = args(lua, a)?;
            match by {
                Value::Integer(n) => app.chat_scroll(Some(n), None),
                Value::Number(n) => app.chat_scroll(Some(n as i64), None),
                Value::String(s) => {
                    let s = s.to_str()?.to_owned();
                    if s != "top" && s != "bottom" {
                        return Err(err(format!(
                            "scroll by rows, \"top\" or \"bottom\", not {s:?}"
                        )));
                    }
                    app.chat_scroll(None, Some(&s));
                }
                other => {
                    return Err(err(format!(
                        "scroll by a number, not {}",
                        other.type_name()
                    )));
                }
            }
            ret(lua, ())
        }
        "prompt_info" => {
            let t = lua.create_table()?;
            t.set("text", app.prompt.text())?;
            t.set(
                "images",
                to_lua(lua, &serde_json::to_value(app.prompt_images()).unwrap())?,
            )?;
            t.set("lines", app.prompt.lines().to_vec())?;
            t.set("cursor", pos_out(lua, app.prompt.cursor())?)?;
            t.set("history", app.prompt_history_active())?;
            if let Some((a, b)) = app.prompt.selection() {
                let sel = lua.create_table()?;
                sel.set("start", pos_out(lua, a)?)?;
                sel.set("end", pos_out(lua, b)?)?;
                sel.set("text", app.prompt.range_text(a, b))?;
                t.set("selection", sel)?;
            }
            ret(lua, t)
        }
        "prompt_offset" => {
            let pos: Value = args(lua, a)?;
            let pos = pos_arg(app, pos)?;
            ret(lua, app.prompt.offset(pos))
        }
        "prompt_position" => {
            let pos: Value = args(lua, a)?;
            let pos = pos_arg(app, pos)?;
            ret(lua, pos_out(lua, pos)?)
        }
        "prompt_get_range" => {
            let (from, to): (Value, Value) = args(lua, a)?;
            let (from, to) = (pos_arg(app, from)?, pos_arg(app, to)?);
            ret(lua, app.prompt.range_text(from, to))
        }
        "prompt_edit" => {
            // Every prompt change from Lua: then the event and a redraw.
            let (op, x, y, text): (String, Value, Value, Option<String>) = args(lua, a)?;
            match op.as_str() {
                "cursor" => {
                    let pos = pos_arg(app, x)?;
                    app.prompt.clear_selection();
                    app.prompt.set_cursor(pos);
                }
                "insert" => {
                    let text = text.ok_or_else(|| err("insert needs text"))?;
                    app.prompt.delete_selection();
                    let at = app.prompt.cursor();
                    app.prompt.replace_range(at, at, &text);
                }
                "range" => {
                    let (from, to) = (pos_arg(app, x)?, pos_arg(app, y)?);
                    let text = text.ok_or_else(|| err("set_range needs text"))?;
                    app.prompt.replace_range(from, to, &text);
                }
                "select" => {
                    let from = pos_arg(app, x)?;
                    let to = match y {
                        Value::Nil => app.prompt.cursor(),
                        y => pos_arg(app, y)?,
                    };
                    app.prompt.select(from, to);
                }
                "unselect" => app.prompt.clear_selection(),
                other => return Err(err(format!("unknown prompt edit {other}"))),
            }
            app.emit_prompt_changed();
            app.dirty = true;
            ret(lua, ())
        }
        other => Err(err(format!("unknown bone API operation: {other}"))),
    }
}

impl bone_ui::lua::Host for App {
    type Line = ratatui::text::Line<'static>;

    fn chat_store(&mut self) -> bone_ui::lua::ChatStore<'_, Self::Line> {
        bone_ui::lua::ChatStore {
            chats: &mut self.chats,
            current: self.current,
            rendering: self.rendering,
            render_deps: &mut self.render_deps,
            render_expires: &mut self.render_expires,
            lua_item_seq: &mut self.lua_item_seq,
        }
    }

    fn owners(&mut self) -> bone_ui::lua::Owners<'_> {
        bone_ui::lua::Owners {
            owner: &mut self.owner,
            callback_owner: &mut self.callback_owner,
            owned: &mut self.owned,
            next_callback: &mut self.next_callback,
        }
    }

    fn option_store(&mut self) -> bone_ui::lua::OptionStore<'_> {
        bone_ui::lua::OptionStore {
            options: &mut self.options,
            dynamic: &mut self.dynamic_options,
            carried: &self.carried_options,
            rev: &mut self.opts_rev,
        }
    }

    fn call_callback_multi(
        &mut self,
        id: u64,
        context: &str,
        args: impl FnOnce(&Lua) -> mlua::Result<MultiValue>,
    ) -> Option<MultiValue> {
        App::call_callback_multi(self, id, context, args)
    }

    fn autocmds(&mut self) -> &mut Vec<Autocmd> {
        &mut self.autocmds
    }

    fn keymap_store(&mut self) -> bone_ui::lua::KeymapStore<'_> {
        bone_ui::lua::KeymapStore {
            keymaps: &mut self.keymaps,
            owner: &mut self.keymap_owner,
        }
    }

    fn context(&self) -> Context {
        App::context(self)
    }

    fn looks(&mut self) -> bone_ui::lua::Looks<'_> {
        bone_ui::lua::Looks {
            theme: &mut self.theme,
            colors_name: &mut self.colors_name,
            rev: &mut self.opts_rev,
        }
    }

    fn config_dir(&self) -> Option<&std::path::Path> {
        self.config_dir.as_deref()
    }

    fn with_api<R>(&mut self, body: impl FnOnce(&Lua) -> mlua::Result<R>) -> mlua::Result<R> {
        App::with_api(self, body)
    }

    fn user_commands(&mut self) -> &mut std::collections::HashMap<String, UserCommand> {
        &mut self.user_commands
    }

    fn call_callback(
        &mut self,
        id: u64,
        context: &str,
        arg: impl FnOnce(&Lua) -> mlua::Result<Value>,
    ) -> Option<Value> {
        App::call_callback(self, id, context, arg)
    }

    fn changed(&mut self) {
        self.dirty = true;
    }

    fn request_raw(
        &mut self,
        method: String,
        params: serde_json::Value,
        then: impl FnOnce(&mut Self, Result<serde_json::Value, String>) + Send + 'static,
    ) {
        App::request_raw(self, method, params, move |app, r| {
            then(app, r.map_err(|e| e.to_string()))
        });
    }

    fn spawn<T: Send + 'static>(
        &mut self,
        fut: impl std::future::Future<Output = T> + Send + 'static,
        then: impl FnOnce(&mut Self, T) + Send + 'static,
    ) {
        App::spawn(self, fut, then);
    }

    fn plugin_files_changing(&mut self) {
        self.rewatch = true;
    }

    fn info(&mut self, text: String) {
        App::info(self, text);
    }

    fn error(&mut self, text: String) {
        App::error(self, text);
    }

    fn lua_error(&mut self, context: &str, e: &mlua::Error) {
        App::lua_error(self, context, e);
    }

    fn settings(&self) -> &serde_json::Value {
        &self.settings
    }

    fn target_chat(&self) -> Option<usize> {
        App::target_chat(self)
    }

    fn prompt_text(&self) -> String {
        App::prompt_text(self)
    }

    fn prompt_images(&self) -> Vec<bone_proto::types::ImageAttachment> {
        App::prompt_images(self).to_vec()
    }

    fn set_prompt_images(
        &mut self,
        images: Vec<bone_proto::types::ImageAttachment>,
    ) -> Result<(), String> {
        App::set_prompt_images(self, images)
    }

    fn focused_panel(&self) -> Option<String> {
        App::focused_panel(self).map(|p| p.id.clone())
    }

    fn process_list(&self) -> Vec<bone_proto::methods::ProcessSnapshot> {
        App::process_list(self)
    }

    fn chat_view(&self) -> serde_json::Value {
        App::chat_view(self)
    }

    fn refresh_ui(&mut self) {
        self.views_rev += 1;
        self.ui_broken.clear();
        self.dirty = true;
    }

    fn has_lua(&self) -> bool {
        self.lua.is_some()
    }

    fn ui_broken(&mut self) -> &mut std::collections::HashSet<String> {
        &mut self.ui_broken
    }

    fn generation(&self) -> (u64, u64) {
        (self.views_rev, self.opts_rev)
    }

    fn chat_expiry(&mut self) -> &mut Option<std::time::Instant> {
        &mut self.chat_expiry
    }
}

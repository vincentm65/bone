//! The TUI's Lua state and the `bone._api` bridge.
//!
//! Lua code only ever runs inside [`App::with_api`], which lends the whole
//! `App` to a scoped dispatch function for the duration of the call. The
//! persistent `bone._api(op, ...)` forwards to whichever dispatcher is
//! current, so API calls act on live state and nested calls (a mapping that
//! runs a command that calls Lua) reborrow safely.

use std::path::PathBuf;

use bone_lua::{Side, from_lua, short_error, to_lua};
use mlua::{FromLuaMulti, Function, IntoLuaMulti, Lua, MultiValue, Table, Value};

use crate::app::App;
use crate::keymap::{Action, Builtin, Context};
use crate::keys;
use crate::options;
use crate::theme::StyleSpec;
use ratatui::style::{Color, Modifier};

/// A color as `parse_color` reads it back.
fn fmt_color(c: Color) -> String {
    match c {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Indexed(n) => n.to_string(),
        other => format!("{other:?}").to_ascii_lowercase(),
    }
}

const DISPATCH: &str = "bone.dispatch";
const CALLBACKS: &str = "bone.callbacks";

/// A user command defined from Lua.
pub struct UserCommand {
    pub callback: u64,
    pub desc: String,
}

pub struct Autocmd {
    pub id: u64,
    pub event: String,
    pub callback: u64,
}

/// Create the TUI Lua state with the runtime API and default keymaps.
pub fn init(app: &mut App, config_dir: Option<PathBuf>) {
    let lua = match setup(config_dir.clone()) {
        Ok(lua) => lua,
        Err(e) => {
            app.error(format!("lua: {}", short_error(&e)));
            return;
        }
    };
    app.lua = Some(lua);
    for rel in ["tui/api.lua", "tui/defaults.lua"] {
        let dir = config_dir.clone();
        if let Err(e) = app.with_api(|lua| bone_lua::run_runtime(lua, dir.as_deref(), rel)) {
            app.lua_error(&format!("runtime/{rel}"), &e);
        }
    }
}

fn setup(config_dir: Option<PathBuf>) -> mlua::Result<Lua> {
    let lua = bone_lua::new_state(Side::Tui, config_dir.as_deref())?;
    lua.set_named_registry_value(CALLBACKS, lua.create_table()?)?;
    let forward = lua.create_function(|lua, args: MultiValue| {
        let dispatch: Value = lua.named_registry_value(DISPATCH)?;
        match dispatch {
            Value::Function(f) => f.call::<MultiValue>(args),
            _ => Err(mlua::Error::runtime(
                "the bone API is only usable from Lua run by bone",
            )),
        }
    })?;
    let bone: Table = lua.globals().get("bone")?;
    bone.set("_api", forward)?;
    bone.set(
        "strwidth",
        lua.create_function(|_, s: String| Ok(crate::text::width(&s)))?,
    )?;
    install_text(&lua, &bone)?;
    // The docs, for /help.
    let docs = lua.create_table()?;
    for (name, text) in DOCS {
        let d = lua.create_table()?;
        d.set("name", *name)?;
        d.set("text", *text)?;
        docs.push(d)?;
    }
    bone.set("_docs", docs)?;
    Ok(lua)
}

/// The user docs, embedded for `/help <topic>`.
const DOCS: &[(&str, &str)] = &[
    ("usage", include_str!("../../../docs/usage.md")),
    ("lua", include_str!("../../../docs/lua.md")),
    ("protocol", include_str!("../../../docs/protocol.md")),
    (
        "architecture",
        include_str!("../../../docs/architecture.md"),
    ),
];

/// Spans from Lua: a string, or a list of strings / `{ text, hl }`.
fn spans_arg(v: Value) -> mlua::Result<Vec<(String, String)>> {
    Ok(crate::ui::parse_items(v, "Normal")?
        .into_iter()
        .map(|i| match i {
            crate::ui::Item::Text(t, h) | crate::ui::Item::Fill(t, h) => (t, h),
        })
        .collect())
}

fn spans_out(lua: &Lua, spans: Vec<(String, String)>) -> mlua::Result<Table> {
    let t = lua.create_table_with_capacity(spans.len(), 0)?;
    for (text, hl) in spans {
        t.push(lua.create_sequence_from([text, hl])?)?;
    }
    Ok(t)
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

/// `bone.markdown` and `bone.text`: pure helpers for views.
fn install_text(lua: &Lua, bone: &Table) -> mlua::Result<()> {
    let md = lua.create_table()?;
    md.set(
        "parse",
        lua.create_function(|lua, text: String| {
            to_lua(
                lua,
                &serde_json::Value::Array(crate::markdown::parse(&text)),
            )
        })?,
    )?;
    bone.set("markdown", md)?;

    let text = lua.create_table()?;
    text.set(
        "width",
        lua.create_function(|_, s: String| Ok(crate::text::width(&crate::text::sanitize(&s))))?,
    )?;
    text.set(
        "truncate",
        lua.create_function(|_, (s, n): (String, usize)| Ok(crate::text::truncate(&s, n)))?,
    )?;
    text.set(
        "clip",
        lua.create_function(|lua, (spans, n): (Value, usize)| {
            spans_out(lua, crate::text::clip_groups(&spans_arg(spans)?, n))
        })?,
    )?;
    text.set(
        "shell",
        lua.create_function(|lua, cmd: String| {
            let spans = crate::shellhl::highlight(&cmd)
                .into_iter()
                .map(|(t, g)| (t, g.to_owned()))
                .collect();
            spans_out(lua, spans)
        })?,
    )?;
    // wrap(spans, width, { first = prefix, rest = prefix, pad = "Group" }):
    // word-wrap to lines; `rest` defaults to blanks as wide as `first`, and
    // `pad` fills each line to the full width (for background bands).
    text.set(
        "wrap",
        lua.create_function(|lua, (spans, width, opts): (Value, usize, Option<Table>)| {
            let spans = spans_arg(spans)?;
            let opt = |k: &str| -> mlua::Result<Option<Value>> {
                Ok(opts
                    .as_ref()
                    .map(|o| o.get::<Value>(k))
                    .transpose()?
                    .filter(|v| !v.is_nil()))
            };
            let first = opt("first")?
                .map(spans_arg)
                .transpose()?
                .unwrap_or_default();
            let rest = match opt("rest")? {
                Some(v) => spans_arg(v)?,
                None => {
                    let w: usize = first.iter().map(|(t, _)| crate::text::width(t)).sum();
                    if w == 0 {
                        Vec::new()
                    } else {
                        vec![(" ".repeat(w), first[0].1.clone())]
                    }
                }
            };
            let pad: Option<String> = opt("pad")?.map(|v| lua.unpack(v)).transpose()?;
            let out = lua.create_table()?;
            for mut line in crate::text::wrap_groups(&spans, &first, &rest, width) {
                if let Some(hl) = &pad {
                    let used: usize = line.iter().map(|(t, _)| crate::text::width(t)).sum();
                    if used < width {
                        line.push((" ".repeat(width - used), hl.clone()));
                    }
                }
                out.push(spans_out(lua, line)?)?;
            }
            Ok(out)
        })?,
    )?;
    bone.set("text", text)?;
    Ok(())
}

impl App {
    /// Run Lua with the API bound to this app.
    pub fn with_api<R>(&mut self, body: impl FnOnce(&Lua) -> mlua::Result<R>) -> mlua::Result<R> {
        let Some(lua) = self.lua.clone() else {
            return Err(mlua::Error::runtime("Lua is not available"));
        };
        let prev: Value = lua.named_registry_value(DISPATCH)?;
        let app: &mut App = self;
        let result = lua.scope(|scope| {
            let f = scope.create_function_mut(|lua, (op, args): (String, MultiValue)| {
                dispatch(app, lua, &op, args)
            })?;
            lua.set_named_registry_value(DISPATCH, f)?;
            body(&lua)
        });
        let _ = lua.set_named_registry_value(DISPATCH, prev);
        result
    }

    pub fn lua_error(&mut self, context: &str, e: &mlua::Error) {
        self.log.push(format!("{context}: {e}"));
        self.error(format!("{context}: {}", short_error(e)));
    }

    /// Load each plugin's `tui.lua`, then `<config dir>/tui.lua`, then fire
    /// `ready`.
    pub fn load_user_config(&mut self) {
        if let Some(dir) = self.config_dir.clone() {
            for plugin in bone_lua::plugins(&dir) {
                let path = plugin.join("tui.lua");
                if let Err(e) = self.with_api(|lua| bone_lua::run_file(lua, &path)) {
                    self.lua_error(&path.display().to_string(), &e);
                }
            }
            let path = dir.join("tui.lua");
            if let Err(e) = self.with_api(|lua| bone_lua::run_file(lua, &path)) {
                self.lua_error(&path.display().to_string(), &e);
            }
        }
        self.fire("ready", serde_json::Value::Null);
    }

    /// Load a colorscheme: `<config dir>/colors/<name>.lua`, a plugin's
    /// `colors/<name>.lua`, or the runtime's.
    pub fn colorscheme(&mut self, name: &str) -> Result<(), String> {
        if name.is_empty() || name.contains(['/', '\\', '.']) {
            return Err(format!("bad colorscheme name {name:?}"));
        }
        let rel = format!("colors/{name}.lua");
        let dir = self.config_dir.clone();
        let mut candidates = Vec::new();
        if let Some(d) = &dir {
            candidates.push(d.join(&rel));
            candidates.extend(bone_lua::plugins(d).into_iter().map(|p| p.join(&rel)));
        }
        let (src, chunk) = candidates
            .iter()
            .find_map(|p| {
                std::fs::read_to_string(p)
                    .ok()
                    .map(|s| (s, p.display().to_string()))
            })
            .or_else(|| bone_lua::runtime_source(dir.as_deref(), &rel))
            .ok_or_else(|| format!("no colorscheme named {name}"))?;
        self.theme.reset();
        self.with_api(|lua| lua.load(src).set_name(format!("@{chunk}")).exec())
            .map_err(|e| format!("colorscheme {name}: {}", short_error(&e)))?;
        self.colors_name = Some(name.to_owned());
        self.opts_rev += 1;
        self.dirty = true;
        Ok(())
    }

    fn store_callback(&mut self, lua: &Lua, f: Function) -> mlua::Result<u64> {
        self.next_callback += 1;
        let id = self.next_callback;
        lua.named_registry_value::<Table>(CALLBACKS)?.set(id, f)?;
        Ok(id)
    }

    fn drop_callback(lua: &Lua, id: u64) -> mlua::Result<()> {
        lua.named_registry_value::<Table>(CALLBACKS)?
            .set(id, Value::Nil)
    }

    /// Call a stored callback with one argument built in Lua.
    pub fn call_callback(
        &mut self,
        id: u64,
        context: &str,
        arg: impl FnOnce(&Lua) -> mlua::Result<Value>,
    ) -> Option<Value> {
        let r = self.with_api(|lua| {
            let f: Function = lua.named_registry_value::<Table>(CALLBACKS)?.get(id)?;
            f.call::<Value>(arg(lua)?)
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
        let cbs: Vec<u64> = self
            .autocmds
            .iter()
            .filter(|a| a.event == event)
            .map(|a| a.callback)
            .collect();
        let mut out = Vec::new();
        for cb in cbs {
            let data = data.clone();
            if let Some(v) =
                self.call_callback(cb, &format!("autocmd {event}"), |lua| to_lua(lua, &data))
            {
                out.push(v);
            }
        }
        out
    }

    pub fn has_autocmd(&self, event: &str) -> bool {
        self.autocmds.iter().any(|a| a.event == event)
    }

    /// `/lua code`, or `/lua =expr` to show a value.
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

fn args<T: FromLuaMulti>(lua: &Lua, args: MultiValue) -> mlua::Result<T> {
    T::from_lua_multi(args, lua)
}

fn ret(lua: &Lua, v: impl IntoLuaMulti) -> mlua::Result<MultiValue> {
    v.into_lua_multi(lua)
}

fn context_arg(opts: &Option<Table>) -> mlua::Result<Context> {
    match opts
        .as_ref()
        .map(|o| o.get::<Option<String>>("context"))
        .transpose()?
        .flatten()
    {
        None => Ok(Context::Main),
        Some(c) => Context::from_name(&c).ok_or_else(|| {
            mlua::Error::runtime(format!("unknown context {c:?} (use main or popup)"))
        }),
    }
}

fn err(msg: impl Into<String>) -> mlua::Error {
    mlua::Error::runtime(msg.into())
}

fn dispatch(app: &mut App, lua: &Lua, op: &str, a: MultiValue) -> mlua::Result<MultiValue> {
    match op {
        "keymap_set" => {
            let (key, rhs, opts): (String, Value, Option<Table>) = args(lua, a)?;
            let ctx = context_arg(&opts)?;
            let action = match rhs {
                Value::String(s) => Action::parse(&s.to_str()?).map_err(err)?,
                Value::Function(f) => Action::Lua(app.store_callback(lua, f)?),
                other => {
                    return Err(err(format!(
                        "keymap action must be a string or function, not {}",
                        other.type_name()
                    )));
                }
            };
            app.keymaps.set(ctx, &key, action).map_err(err)?;
            ret(lua, ())
        }
        "keymap_del" => {
            let (key, opts): (String, Option<Table>) = args(lua, a)?;
            app.keymaps.del(context_arg(&opts)?, &key).map_err(err)?;
            ret(lua, ())
        }
        "cmd" => {
            let line: String = args(lua, a)?;
            app.execute(&line);
            ret(lua, ())
        }
        "command_create" => {
            let (name, f, opts): (String, Function, Option<Table>) = args(lua, a)?;
            let valid = name.starts_with(|c: char| c.is_ascii_lowercase())
                && name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
            if !valid {
                return Err(err(format!(
                    "command names are lowercase letters, digits, - and _: {name:?}"
                )));
            }
            if crate::commands::resolve(&name).is_some() {
                return Err(err(format!("/{name} is a built-in command")));
            }
            let desc = opts
                .map(|o| o.get::<Option<String>>("desc"))
                .transpose()?
                .flatten()
                .unwrap_or_default();
            let callback = app.store_callback(lua, f)?;
            if let Some(old) = app
                .user_commands
                .insert(name, UserCommand { callback, desc })
            {
                App::drop_callback(lua, old.callback)?;
            }
            ret(lua, ())
        }
        "command_del" => {
            let name: String = args(lua, a)?;
            let old = app
                .user_commands
                .remove(&name)
                .ok_or_else(|| err(format!("no user command {name}")))?;
            App::drop_callback(lua, old.callback)?;
            ret(lua, ())
        }
        "opt_get" => {
            let name: String = args(lua, a)?;
            match app
                .options
                .get(&name)
                .ok_or_else(|| err(format!("unknown option: {name}")))?
            {
                options::Value::Bool(b) => ret(lua, b),
                options::Value::Number(n) => ret(lua, n),
            }
        }
        "opt_set" => {
            let (name, value): (String, Value) = args(lua, a)?;
            let value = match value {
                Value::Boolean(b) => options::Value::Bool(b),
                Value::Integer(i) if i >= 0 => options::Value::Number(i as u64),
                Value::Number(n) if n >= 0.0 && n.fract() == 0.0 => {
                    options::Value::Number(n as u64)
                }
                other => return Err(err(format!("bad value for {name}: {}", other.type_name()))),
            };
            app.options.set(&name, value).map_err(err)?;
            app.opts_rev += 1;
            app.dirty = true;
            ret(lua, ())
        }
        "on" => {
            let (event, f): (String, Function) = args(lua, a)?;
            let callback = app.store_callback(lua, f)?;
            app.next_callback += 1;
            let id = app.next_callback;
            app.autocmds.push(Autocmd {
                id,
                event,
                callback,
            });
            ret(lua, id)
        }
        "off" => {
            let id: u64 = args(lua, a)?;
            if let Some(i) = app.autocmds.iter().position(|a| a.id == id) {
                let a = app.autocmds.remove(i);
                App::drop_callback(lua, a.callback)?;
            }
            ret(lua, ())
        }
        "request" => {
            let (method, params, f): (String, Value, Option<Function>) = args(lua, a)?;
            let params = from_lua(&params)?;
            let cb = f.map(|f| app.store_callback(lua, f)).transpose()?;
            app.request_raw(method.clone(), params, move |app, r| {
                let Some(cb) = cb else {
                    if let Err(e) = r {
                        app.error(format!("{method}: {e}"));
                    }
                    return;
                };
                let called = app.with_api(|lua| {
                    let cbs: Table = lua.named_registry_value(CALLBACKS)?;
                    let f: Function = cbs.get(cb)?;
                    cbs.set(cb, Value::Nil)?;
                    match r {
                        Ok(v) => f.call::<()>(to_lua(lua, &v)?),
                        Err(e) => f.call::<()>((Value::Nil, e.to_string())),
                    }
                });
                if let Err(e) = called {
                    app.lua_error(&format!("{method} callback"), &e);
                }
            });
            ret(lua, ())
        }
        "wait" => {
            // Background work (bone.system, bone.http, bone.defer), then the
            // callback with the result, or (nil, error).
            let (spec, f): (Value, Option<Function>) = args(lua, a)?;
            let spec = from_lua(&spec)?;
            let cb = f.map(|f| app.store_callback(lua, f)).transpose()?;
            app.spawn(bone_lua::wait::run(spec), move |app, r| {
                let Some(cb) = cb else {
                    if let Err(e) = r {
                        app.error(e);
                    }
                    return;
                };
                let called = app.with_api(|lua| {
                    let cbs: Table = lua.named_registry_value(CALLBACKS)?;
                    let f: Function = cbs.get(cb)?;
                    cbs.set(cb, Value::Nil)?;
                    match r {
                        Ok(v) => f.call::<()>(to_lua(lua, &v)?),
                        Err(e) => f.call::<()>((Value::Nil, e)),
                    }
                });
                if let Err(e) = called {
                    app.lua_error("callback", &e);
                }
            });
            ret(lua, ())
        }
        "notify" => {
            let (msg, level): (String, Option<String>) = args(lua, a)?;
            if level.as_deref() == Some("error") {
                app.error(msg);
            } else {
                app.info(msg);
            }
            ret(lua, ())
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
        "prompt_get" => ret(lua, app.prompt_text()),
        "prompt_set" => {
            let text: String = args(lua, a)?;
            app.set_prompt_text(&text);
            ret(lua, ())
        }
        "session" => {
            let Some(c) = app.target_chat().and_then(|b| app.chat(b)) else {
                return ret(lua, Value::Nil);
            };
            let Some(s) = &c.session else {
                return ret(lua, Value::Nil);
            };
            let t = lua.create_table()?;
            t.set("session_id", s.session_id.as_str())?;
            t.set("cwd", s.cwd.as_str())?;
            t.set("title", s.title.clone())?;
            t.set("running", c.turn.is_some())?;
            ret(lua, t)
        }
        "health" => ret(
            lua,
            to_lua(lua, &serde_json::Value::Array(app.tui_health()))?,
        ),
        "open_session" => {
            let id: String = args(lua, a)?;
            app.open_session(id);
            ret(lua, ())
        }
        "popup_open" => {
            let spec: Table = args(lua, a)?;
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
            app.popups.push(p);
            app.dirty = true;
            ret(lua, id)
        }
        "popup_update" => {
            let (id, spec): (u64, Table) = args(lua, a)?;
            let Some(i) = app.popups.iter().position(|p| p.id == id) else {
                return ret(lua, false);
            };
            let mut p = app.popups.remove(i);
            let r = apply_win_spec(app, lua, &mut p, &spec);
            app.popups.insert(i, p);
            r?;
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
                let p = app.popups.remove(i);
                App::drop_callback(lua, p.lines)?;
                if let Some(cb) = p.on_key {
                    App::drop_callback(lua, cb)?;
                }
                for (_, cb) in p.keys {
                    App::drop_callback(lua, cb)?;
                }
                app.dirty = true;
            }
            ret(lua, ())
        }
        "ui_refresh" => {
            app.views_rev += 1;
            app.ui_broken.clear();
            app.dirty = true;
            ret(lua, ())
        }
        "chat_items" => {
            let opts: Option<Table> = args(lua, a)?;
            let kind: Option<String> = opts.as_ref().map(|o| o.get("kind")).transpose()?.flatten();
            let last: Option<usize> = opts.as_ref().map(|o| o.get("last")).transpose()?.flatten();
            let items = app.chat_items(kind.as_deref(), last);
            ret(lua, to_lua(lua, &serde_json::Value::Array(items))?)
        }
        "hl_set" => {
            let (name, spec): (String, Table) = args(lua, a)?;
            let spec = StyleSpec {
                fg: spec.get("fg")?,
                bg: spec.get("bg")?,
                bold: spec.get::<Option<bool>>("bold")?.unwrap_or(false),
                italic: spec.get::<Option<bool>>("italic")?.unwrap_or(false),
                underline: spec.get::<Option<bool>>("underline")?.unwrap_or(false),
                reverse: spec.get::<Option<bool>>("reverse")?.unwrap_or(false),
                dim: spec.get::<Option<bool>>("dim")?.unwrap_or(false),
                link: spec.get("link")?,
            };
            let style = spec
                .to_style(&app.theme)
                .map_err(|e| err(format!("{name}: {e}")))?;
            app.theme.set(&name, style);
            app.opts_rev += 1;
            app.dirty = true;
            ret(lua, ())
        }
        "hl_get" => {
            let name: String = args(lua, a)?;
            let st = app.theme.hl(&name);
            let t = lua.create_table()?;
            t.set("fg", st.fg.map(fmt_color))?;
            t.set("bg", st.bg.map(fmt_color))?;
            for (key, m) in [
                ("bold", Modifier::BOLD),
                ("italic", Modifier::ITALIC),
                ("underline", Modifier::UNDERLINED),
                ("reverse", Modifier::REVERSED),
                ("dim", Modifier::DIM),
            ] {
                if st.add_modifier.contains(m) {
                    t.set(key, true)?;
                }
            }
            ret(lua, t)
        }
        "hl_reset" => {
            app.theme.reset();
            app.opts_rev += 1;
            app.dirty = true;
            ret(lua, ())
        }
        "hl_names" => ret(
            lua,
            app.theme
                .names()
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>(),
        ),
        "colorscheme" => {
            let name: String = args(lua, a)?;
            app.colorscheme(&name).map_err(err)?;
            ret(lua, ())
        }
        other => Err(err(format!("unknown bone API operation: {other}"))),
    }
}

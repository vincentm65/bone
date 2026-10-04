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
use crate::options::{self, DynamicKind, DynamicOption, DynamicValue};
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
/// Names in `package.loaded` once the API is up; a reload unloads the rest.
const API_MODULES: &str = "bone.api_modules";

/// A user command defined from Lua.
#[derive(Clone)]
pub struct UserCommand {
    pub callback: u64,
    pub desc: String,
    pub aliases: Vec<String>,
    pub completion: Option<u64>,
    pub args: Option<serde_json::Value>,
}

pub struct Autocmd {
    pub id: u64,
    pub event: String,
    pub callback: u64,
}

fn canonical_event(name: &str) -> &str {
    match name {
        "prompt" | "prompt_changed" | "prompt/changed" => "prompt/changed",
        "focus" | "focus_changed" | "focus/changed" => "focus/changed",
        "ui/resize" | "resize" => "resize",
        "key/pressed" | "key" => "key",
        "paste" | "text/pasted" => "paste",
        "panel" | "panel/opened" | "popup" | "popup/opened" => "panel/opened",
        "panel/closed" | "popup/closed" => "panel/closed",
        "panel/updated" | "popup/updated" => "panel/updated",
        other => other,
    }
}

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
pub fn init(app: &mut App, config_dir: Option<PathBuf>) {
    let lua = match setup(config_dir.clone()) {
        Ok(lua) => lua,
        Err(e) => {
            app.error(format!("lua: {}", short_error(&e)));
            return;
        }
    };
    app.lua = Some(lua);
    let dir = config_dir.clone();
    if let Err(e) = app.with_api(|lua| {
        bone_lua::run_runtime(lua, dir.as_deref(), "tui/api.lua")?;
        let package: Table = lua.globals().get("package")?;
        let loaded: Table = package.get("loaded")?;
        let names = lua.create_table()?;
        for pair in loaded.pairs::<Value, Value>() {
            names.set(pair?.0, true)?;
        }
        lua.set_named_registry_value(API_MODULES, names)
    }) {
        app.lua_error("runtime/tui/api.lua", &e);
    }
    app.run_defaults();
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
        "now",
        lua.create_function(|_, ()| {
            Ok(std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0.0, |d| d.as_secs_f64() * 1000.0))
        })?,
    )?;
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

/// `bone.chat.items` options.
fn item_filter(opts: &Option<Table>) -> mlua::Result<crate::data::ItemFilter> {
    let Some(o) = opts else {
        return Ok(Default::default());
    };
    Ok(crate::data::ItemFilter {
        kind: o.get("kind")?,
        name: o.get("name")?,
        turn: o.get("turn")?,
        running: o.get("running")?,
        error: o.get("error")?,
        from: o.get("from")?,
        to: o.get("to")?,
        first: o.get("first")?,
        last: o.get("last")?,
        session: o.get("session")?,
    })
}

/// A table of fields for a Lua chat item, as a JSON object.
fn lua_fields(v: Value) -> mlua::Result<serde_json::Map<String, serde_json::Value>> {
    match v {
        Value::Nil => Ok(Default::default()),
        Value::Table(_) => match from_lua(&v)? {
            serde_json::Value::Object(m) => Ok(m),
            serde_json::Value::Array(a) if a.is_empty() => Ok(Default::default()),
            _ => Err(err("chat item fields must be a table with string keys")),
        },
        other => Err(err(format!(
            "chat item fields must be a table, not {}",
            other.type_name()
        ))),
    }
}

fn session_opt(opts: &Option<Table>) -> mlua::Result<Option<String>> {
    match opts {
        Some(o) => o.get("session"),
        None => Ok(None),
    }
}

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
    Ok(Spec {
        command,
        cwd: opts.as_ref().map(|o| o.get("cwd")).transpose()?.flatten(),
        env,
        stdin,
        timeout,
    })
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

    /// Load each plugin's `tui.lua` (as that plugin), then
    /// `<config dir>/tui.lua`, then a trusted project's `.bone/tui.lua`, then
    /// fire `ready`.
    pub fn load_user_config(&mut self) {
        if let Some(dir) = self.config_dir.clone() {
            for plugin in bone_lua::plugins(&dir) {
                let name = bone_lua::plugin_name(&plugin);
                self.load_plugin(&name, &plugin, crate::plugins::Kind::Plugin);
            }
            let path = dir.join("tui.lua");
            if let Err(e) = self.with_api(|lua| bone_lua::run_file(lua, &path)) {
                self.lua_error(&path.display().to_string(), &e);
            }
            self.load_project();
        }
        self.fire("ready", serde_json::Value::Null);
    }

    fn run_defaults(&mut self) {
        let dir = self.config_dir.clone();
        let rel = "tui/defaults.lua";
        if let Err(e) = self.with_api(|lua| bone_lua::run_runtime(lua, dir.as_deref(), rel)) {
            self.lua_error(&format!("runtime/{rel}"), &e);
        }
    }

    /// Re-source Lua customization after a file change. This intentionally
    /// keeps the Lua state and Rust session alive, matching Vim's `:source`
    /// model; a later cleanup API can make plugin reloads fully idempotent.
    pub fn reload_user_config(&mut self) {
        // The defaults and `tui.lua` are sourced again. Clear mutable UI
        // customization first so deleting a line really removes its previous
        // definition, and forget every module required since the API was set
        // up (the standard UI, menu and commands included) so edits to them
        // load. The API tables stay alive; Rust still owns the session.
        let reset = self.with_api(|lua| {
            let keep: Table = lua.named_registry_value(API_MODULES)?;
            lua.load(
                r#"
                local keep = ...
                bone.ui._reset()
                for name in pairs(package.loaded) do
                    if not keep[name] then
                        package.loaded[name] = nil
                    end
                end
                "#,
            )
            .call::<()>(keep)
        });
        if let Err(e) = reset {
            self.lua_error("resetting Lua UI", &e);
            return;
        }
        self.ui_broken.clear();
        self.spinner = Default::default();
        self.run_defaults();
        self.load_user_config();
        self.dirty = true;
        self.info("Lua configuration reloaded");
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

    /// Keep `f` for later; it runs as the plugin that stored it.
    fn store_callback(&mut self, lua: &Lua, f: Function) -> mlua::Result<u64> {
        self.next_callback += 1;
        let id = self.next_callback;
        lua.named_registry_value::<Table>(CALLBACKS)?.set(id, f)?;
        if let Some(owner) = &self.owner {
            self.callback_owner.insert(id, owner.clone());
        }
        Ok(id)
    }

    /// Run `f` as the plugin that owns callback `cb` (or as the user's
    /// config), so what it creates belongs to that plugin.
    pub fn as_owner_of<R>(&mut self, cb: u64, f: impl FnOnce(&mut App) -> R) -> R {
        let owner = self.callback_owner.get(&cb).cloned();
        let prev = std::mem::replace(&mut self.owner, owner);
        let r = f(self);
        self.owner = prev;
        r
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
        let event = canonical_event(event);
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
        let event = canonical_event(event);
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
            mlua::Error::runtime(format!(
                "invalid context {c:?} (use main, popup, or letters, digits, _, - and .)"
            ))
        }),
    }
}

fn context_from_name(name: String) -> mlua::Result<Context> {
    Context::from_name(&name)
        .ok_or_else(|| err(format!("invalid context {name:?} (bad context name)")))
}

fn fallback_arg(opts: &Option<Table>) -> mlua::Result<Option<Vec<Context>>> {
    let Some(opts) = opts else { return Ok(None) };
    let value = match opts.get::<Value>("fallback")? {
        Value::Nil => opts.get::<Value>("fallbacks")?,
        value => value,
    };
    match value {
        Value::Nil => Ok(None),
        Value::String(name) => Ok(Some(vec![context_from_name(name.to_str()?.to_owned())?])),
        Value::Table(table) => table
            .sequence_values::<String>()
            .map(|name| name.and_then(context_from_name))
            .collect::<mlua::Result<Vec<_>>>()
            .map(Some),
        other => Err(err(format!(
            "context fallback must be a name or list, not {}",
            other.type_name()
        ))),
    }
}

fn raw_context_arg(opts: &Option<Table>) -> mlua::Result<Option<Context>> {
    let Some(opts) = opts else { return Ok(None) };
    match opts.get::<Option<String>>("context")? {
        Some(name) => context_from_name(name).map(Some),
        None => Ok(None),
    }
}

fn err(msg: impl Into<String>) -> mlua::Error {
    mlua::Error::runtime(msg.into())
}

fn dynamic_from_lua(
    value: Value,
    expected: Option<DynamicKind>,
) -> mlua::Result<(DynamicKind, DynamicValue)> {
    let inferred = match value {
        Value::Boolean(value) => (DynamicKind::Boolean, DynamicValue::Boolean(value)),
        Value::Integer(value) => (DynamicKind::Integer, DynamicValue::Integer(value)),
        Value::Number(value) if value.is_finite() => {
            (DynamicKind::Number, DynamicValue::Number(value))
        }
        Value::String(value) => (
            DynamicKind::String,
            DynamicValue::String(value.to_str()?.to_owned()),
        ),
        other => {
            return Err(err(format!(
                "option values must be boolean, number or string, not {}",
                other.type_name()
            )));
        }
    };
    let Some(expected) = expected else {
        return Ok(inferred);
    };
    let value = match (expected, inferred.1) {
        (DynamicKind::Boolean, DynamicValue::Boolean(value)) => DynamicValue::Boolean(value),
        (DynamicKind::Integer, DynamicValue::Integer(value)) => DynamicValue::Integer(value),
        (DynamicKind::Integer, DynamicValue::Number(value))
            if value.fract() == 0.0 && value >= i64::MIN as f64 && value <= i64::MAX as f64 =>
        {
            DynamicValue::Integer(value as i64)
        }
        (DynamicKind::Number, DynamicValue::Integer(value)) => DynamicValue::Number(value as f64),
        (DynamicKind::Number, DynamicValue::Number(value)) => DynamicValue::Number(value),
        (DynamicKind::String, DynamicValue::String(value)) => DynamicValue::String(value),
        (expected, actual) => {
            return Err(err(format!(
                "option expects {}, got {}",
                expected.name(),
                actual.type_name()
            )));
        }
    };
    Ok((expected, value))
}

fn dynamic_to_lua(lua: &Lua, value: &DynamicValue) -> mlua::Result<Value> {
    Ok(match value {
        DynamicValue::Boolean(value) => Value::Boolean(*value),
        DynamicValue::Integer(value) => Value::Integer(*value),
        DynamicValue::Number(value) => Value::Number(*value),
        DynamicValue::String(value) => Value::String(lua.create_string(value)?),
    })
}

fn dynamic_from_text(text: &str, kind: DynamicKind, name: &str) -> Result<DynamicValue, String> {
    match kind {
        DynamicKind::Boolean => match text {
            "true" | "on" | "yes" | "1" => Ok(DynamicValue::Boolean(true)),
            "false" | "off" | "no" | "0" => Ok(DynamicValue::Boolean(false)),
            _ => Err(format!("{name} takes true or false")),
        },
        DynamicKind::Integer => text
            .parse::<i64>()
            .map(DynamicValue::Integer)
            .map_err(|_| format!("{name} takes an integer")),
        DynamicKind::Number => text
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite())
            .map(DynamicValue::Number)
            .ok_or_else(|| format!("{name} takes a finite number")),
        DynamicKind::String => Ok(DynamicValue::String(text.to_owned())),
    }
}

enum DynamicOptionArg<'a> {
    Query(&'a str),
    Set(&'a str, &'a str),
    Enable(&'a str),
    Disable(&'a str),
    Toggle(&'a str),
}

fn dynamic_option_arg<'a>(app: &App, arg: &'a str) -> Option<DynamicOptionArg<'a>> {
    if let Some(name) = arg.strip_suffix('?')
        && app.dynamic_options.contains_key(name)
    {
        return Some(DynamicOptionArg::Query(name));
    }
    if let Some((name, value)) = arg.split_once('=')
        && app.dynamic_options.contains_key(name)
    {
        return Some(DynamicOptionArg::Set(name, value));
    }
    if app.dynamic_options.contains_key(arg) {
        return Some(DynamicOptionArg::Enable(arg));
    }
    if let Some(name) = arg.strip_prefix("no")
        && app.dynamic_options.contains_key(name)
    {
        return Some(DynamicOptionArg::Disable(name));
    }
    if let Some(name) = arg.strip_prefix("inv")
        && app.dynamic_options.contains_key(name)
    {
        return Some(DynamicOptionArg::Toggle(name));
    }
    if let Some(name) = arg.strip_suffix('!')
        && app.dynamic_options.contains_key(name)
    {
        return Some(DynamicOptionArg::Toggle(name));
    }
    None
}

/// Apply one `/set` argument to a dynamic option. `None` means the argument
/// belongs to the fixed Rust options instead.
pub(crate) fn apply_dynamic_option(
    app: &mut App,
    arg: &str,
) -> Option<Result<Option<String>, String>> {
    let operation = dynamic_option_arg(app, arg)?;
    Some((|| {
        let (name, kind, current) = match &operation {
            DynamicOptionArg::Query(name)
            | DynamicOptionArg::Set(name, _)
            | DynamicOptionArg::Enable(name)
            | DynamicOptionArg::Disable(name)
            | DynamicOptionArg::Toggle(name) => {
                let option = app
                    .dynamic_options
                    .get(*name)
                    .expect("dynamic option found");
                ((*name).to_owned(), option.kind, option.value.clone())
            }
        };
        let new = match operation {
            DynamicOptionArg::Query(_) => return Ok(Some(format!("{name}={current}"))),
            DynamicOptionArg::Set(_, value) => dynamic_from_text(value, kind, &name)?,
            DynamicOptionArg::Enable(_) => {
                if kind != DynamicKind::Boolean {
                    return Err(format!("{name} is not a boolean; use {name}=VALUE"));
                }
                DynamicValue::Boolean(true)
            }
            DynamicOptionArg::Disable(_) => {
                if kind != DynamicKind::Boolean {
                    return Err(format!("{name} is not a boolean; use {name}=VALUE"));
                }
                DynamicValue::Boolean(false)
            }
            DynamicOptionArg::Toggle(_) => {
                let DynamicValue::Boolean(value) = current else {
                    return Err(format!("{name} is not a boolean; use {name}=VALUE"));
                };
                DynamicValue::Boolean(!value)
            }
        };
        if current == new {
            return Ok(None);
        }
        let callback = {
            let option = app
                .dynamic_options
                .get_mut(name.as_str())
                .expect("dynamic option found");
            option.value = new.clone();
            option.on_change
        };
        if let Some(callback) = callback {
            app.call_callback_multi(callback, &format!("option {name}"), |lua| {
                Ok(vec![dynamic_to_lua(lua, &new)?, dynamic_to_lua(lua, &current)?].into())
            });
        }
        Ok(None)
    })())
}

fn valid_dynamic_name(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
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
            let seq = keys::parse_sequence(&key).map_err(err)?;
            app.keymaps.set(ctx.clone(), &key, action).map_err(err)?;
            match app.owner.clone() {
                Some(owner) => {
                    app.keymap_owner.insert((ctx, seq), owner);
                }
                None => {
                    app.keymap_owner.remove(&(ctx, seq));
                }
            }
            ret(lua, ())
        }
        "keymap_del" => {
            let (key, opts): (String, Option<Table>) = args(lua, a)?;
            let ctx = context_arg(&opts)?;
            app.keymaps.del(ctx.clone(), &key).map_err(err)?;
            app.keymap_owner
                .remove(&(ctx, keys::parse_sequence(&key).map_err(err)?));
            ret(lua, ())
        }
        "keymap_context" => {
            let (name, opts): (String, Option<Table>) = args(lua, a)?;
            let ctx = context_from_name(name)?;
            if !app.keymaps.has_context(&ctx) {
                app.own(crate::plugins::Owned::Context(ctx.clone()));
            }
            let fallback = fallback_arg(&opts)?;
            let priority = opts
                .as_ref()
                .map(|o| o.get::<Option<i32>>("priority"))
                .transpose()?
                .flatten();
            app.keymaps.define(ctx, fallback, priority).map_err(err)?;
            ret(lua, ())
        }
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
        "keymap_current" => ret(lua, app.context().name().to_owned()),
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
            let aliases = match opts.as_ref().map(|o| o.get::<Value>("aliases")) {
                None | Some(Ok(Value::Nil)) => Vec::new(),
                Some(Ok(Value::String(alias))) => vec![alias.to_str()?.to_owned()],
                Some(Ok(Value::Table(aliases))) => aliases
                    .sequence_values::<String>()
                    .collect::<mlua::Result<Vec<_>>>()?,
                Some(Ok(other)) => {
                    return Err(err(format!(
                        "command aliases must be a string or list, not {}",
                        other.type_name()
                    )));
                }
                Some(Err(e)) => return Err(e),
            };
            if app.user_commands.iter().any(|(old_name, old)| {
                old_name != &name && old.aliases.iter().any(|alias| alias == &name)
            }) {
                return Err(err(format!("/{name} is already a user command")));
            }
            for (index, alias) in aliases.iter().enumerate() {
                // Any word without spaces or "/" (so "?" can be /help's).
                let valid = !alias.is_empty()
                    && !alias.contains(char::is_whitespace)
                    && !alias.contains('/');
                if !valid {
                    return Err(err(format!("invalid command alias {alias:?}")));
                }
                if alias == &name || aliases[..index].contains(alias) {
                    return Err(err(format!("/{alias} is already a command")));
                }
                if app.user_commands.iter().any(|(old_name, old)| {
                    old_name != &name
                        && (old_name == alias || old.aliases.iter().any(|a| a == alias))
                }) {
                    return Err(err(format!("/{alias} is already a user command")));
                }
            }
            let args_spec = match opts.as_ref().map(|o| o.get::<Value>("args")) {
                None | Some(Ok(Value::Nil)) => None,
                Some(Ok(Value::Table(spec))) => {
                    let value = from_lua(&Value::Table(spec))?;
                    crate::commands::validate_argument_specs(&value).map_err(err)?;
                    Some(value)
                }
                Some(Ok(other)) => {
                    return Err(err(format!(
                        "command args must be a table, not {}",
                        other.type_name()
                    )));
                }
                Some(Err(e)) => return Err(e),
            };
            let desc = opts
                .as_ref()
                .map(|o| o.get::<Option<String>>("desc"))
                .transpose()?
                .flatten()
                .unwrap_or_default();
            let completion = opts
                .as_ref()
                .map(|o| {
                    let complete: Option<Function> = o.get("complete")?;
                    if complete.is_some() {
                        Ok(complete)
                    } else {
                        o.get("completion")
                    }
                })
                .transpose()?
                .flatten()
                .map(|f| app.store_callback(lua, f))
                .transpose()?;
            let callback = app.store_callback(lua, f)?;
            app.own(crate::plugins::Owned::Command(name.clone(), callback));
            if let Some(old) = app.user_commands.insert(
                name,
                UserCommand {
                    callback,
                    desc,
                    aliases,
                    completion,
                    args: args_spec,
                },
            ) {
                App::drop_callback(lua, old.callback)?;
                if let Some(completion) = old.completion {
                    App::drop_callback(lua, completion)?;
                }
            }
            ret(lua, ())
        }
        "command_del" => {
            let name: String = args(lua, a)?;
            let canonical = app
                .user_commands
                .iter()
                .find(|(canonical, command)| {
                    canonical.as_str() == name || command.aliases.iter().any(|alias| alias == &name)
                })
                .map(|(canonical, _)| canonical.clone())
                .ok_or_else(|| err(format!("no user command {name}")))?;
            let old = app.user_commands.remove(&canonical).expect("command found");
            App::drop_callback(lua, old.callback)?;
            if let Some(completion) = old.completion {
                App::drop_callback(lua, completion)?;
            }
            ret(lua, ())
        }
        "opt_get" => {
            let name: String = args(lua, a)?;
            if let Some(value) = app.options.get(&name) {
                return match value {
                    options::Value::Bool(b) => ret(lua, b),
                    options::Value::Number(n) => ret(lua, n),
                };
            }
            let value = app
                .dynamic_options
                .get(&name)
                .ok_or_else(|| err(format!("unknown option: {name}")))?
                .value
                .clone();
            ret(lua, dynamic_to_lua(lua, &value)?)
        }
        "opt_set" => {
            let (name, value): (String, Value) = args(lua, a)?;
            if app.options.get(&name).is_some() {
                let value = match value {
                    Value::Boolean(b) => options::Value::Bool(b),
                    Value::Integer(i) if i >= 0 => options::Value::Number(i as u64),
                    Value::Number(n) if n >= 0.0 && n.fract() == 0.0 => {
                        options::Value::Number(n as u64)
                    }
                    other => {
                        return Err(err(format!("bad value for {name}: {}", other.type_name())));
                    }
                };
                app.options.set(&name, value).map_err(err)?;
            } else {
                let kind = app
                    .dynamic_options
                    .get(&name)
                    .ok_or_else(|| err(format!("unknown option: {name}")))?
                    .kind;
                let (_, new_value) = dynamic_from_lua(value, Some(kind))?;
                let (old_value, callback) = {
                    let option = app.dynamic_options.get_mut(&name).expect("option found");
                    let old = option.value.clone();
                    option.value = new_value.clone();
                    (old, option.on_change)
                };
                if old_value != new_value
                    && let Some(callback) = callback
                {
                    let new_value = new_value.clone();
                    app.call_callback_multi(callback, &format!("option {name}"), |lua| {
                        Ok(vec![
                            dynamic_to_lua(lua, &new_value)?,
                            dynamic_to_lua(lua, &old_value)?,
                        ]
                        .into())
                    });
                }
            }
            app.opts_rev += 1;
            app.dirty = true;
            ret(lua, ())
        }
        "opt_define" => {
            let (name, default, opts): (String, Value, Option<Table>) = args(lua, a)?;
            if !valid_dynamic_name(&name) {
                return Err(err(format!("invalid option name {name:?}")));
            }
            if options::NAMES.contains(&name.as_str()) {
                return Err(err(format!("{name} is a built-in option")));
            }
            // Re-sourcing the file that defined an option keeps its value.
            let previous = match app.dynamic_options.get(&name) {
                Some(old) if old.owner == app.owner => Some(old.value.clone()),
                Some(_) => return Err(err(format!("option already defined: {name}"))),
                None => None,
            };
            let requested = opts
                .as_ref()
                .map(|o| o.get::<Option<String>>("type"))
                .transpose()?
                .flatten()
                .map(|name| {
                    DynamicKind::parse(&name)
                        .ok_or_else(|| err(format!("unknown option type {name:?}")))
                })
                .transpose()?;
            let (kind, value) = dynamic_from_lua(default, requested)?;
            let desc = opts
                .as_ref()
                .map(|o| o.get::<Option<String>>("desc"))
                .transpose()?
                .flatten()
                .unwrap_or_default();
            let callback = opts
                .as_ref()
                .map(|o| o.get::<Option<Function>>("on_change"))
                .transpose()?
                .flatten()
                .map(|f| app.store_callback(lua, f))
                .transpose()?;
            let current = previous
                .filter(|v| v.kind() == kind)
                .unwrap_or_else(|| value.clone());
            app.own(crate::plugins::Owned::Option(name.clone()));
            if let Some(old) = app.dynamic_options.get(&name).and_then(|o| o.on_change) {
                App::drop_callback(lua, old)?;
            }
            app.dynamic_options.insert(
                name,
                DynamicOption {
                    value: current,
                    default: value,
                    kind,
                    desc,
                    on_change: callback,
                    owner: app.owner.clone(),
                },
            );
            app.opts_rev += 1;
            app.dirty = true;
            ret(lua, ())
        }
        "opt_del" => {
            let name: String = args(lua, a)?;
            let Some(option) = app.dynamic_options.remove(&name) else {
                return ret(lua, false);
            };
            if let Some(callback) = option.on_change {
                App::drop_callback(lua, callback)?;
            }
            app.opts_rev += 1;
            app.dirty = true;
            ret(lua, true)
        }
        "opt_names" => {
            let mut names: Vec<String> = options::NAMES
                .iter()
                .map(|name| (*name).to_owned())
                .collect();
            names.extend(app.dynamic_options.keys().cloned());
            names.sort();
            ret(lua, names)
        }
        "opt_info" => {
            let name: String = args(lua, a)?;
            let table = lua.create_table()?;
            if let Some(value) = app.options.get(&name) {
                let (kind, value) = match value {
                    options::Value::Bool(value) => ("boolean", Value::Boolean(value)),
                    options::Value::Number(value) => ("integer", Value::Integer(value as i64)),
                };
                table.set("type", kind)?;
                table.set("value", value)?;
            } else if let Some(option) = app.dynamic_options.get(&name) {
                table.set("type", option.kind.name())?;
                table.set("value", dynamic_to_lua(lua, &option.value)?)?;
                table.set("default", dynamic_to_lua(lua, &option.default)?)?;
                table.set("desc", option.desc.as_str())?;
            } else {
                return Err(err(format!("unknown option: {name}")));
            }
            ret(lua, table)
        }
        "on" => {
            let (event, f): (String, Function) = args(lua, a)?;
            let callback = app.store_callback(lua, f)?;
            app.next_callback += 1;
            let id = app.next_callback;
            let event = canonical_event(&event).to_owned();
            app.autocmds.push(Autocmd {
                id,
                event,
                callback,
            });
            app.own(crate::plugins::Owned::Autocmd(id));
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
                let called = app.as_owner_of(cb, |app| {
                    app.with_api(|lua| {
                        let cbs: Table = lua.named_registry_value(CALLBACKS)?;
                        let f: Function = cbs.get(cb)?;
                        cbs.set(cb, Value::Nil)?;
                        match r {
                            Ok(v) => f.call::<()>(to_lua(lua, &v)?),
                            Err(e) => f.call::<()>((Value::Nil, e.to_string())),
                        }
                    })
                });
                app.callback_owner.remove(&cb);
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
                let called = app.as_owner_of(cb, |app| {
                    app.with_api(|lua| {
                        let cbs: Table = lua.named_registry_value(CALLBACKS)?;
                        let f: Function = cbs.get(cb)?;
                        cbs.set(cb, Value::Nil)?;
                        match r {
                            Ok(v) => f.call::<()>(to_lua(lua, &v)?),
                            Err(e) => f.call::<()>((Value::Nil, e)),
                        }
                    })
                });
                app.callback_owner.remove(&cb);
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
            let opt = |k: &str| -> mlua::Result<Option<Value>> {
                Ok(opts
                    .as_ref()
                    .map(|o| o.get::<Value>(k))
                    .transpose()?
                    .filter(|v| !v.is_nil()))
            };
            let mut stored = Vec::new();
            let mut callback = |app: &mut App, k: &str| -> mlua::Result<Option<u64>> {
                match opt(k)? {
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
                    name: opts.as_ref().map(|o| o.get("name")).transpose()?.flatten(),
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
        "command_list" => {
            let mut list: Vec<serde_json::Value> = app
                .user_commands
                .iter()
                .map(|(name, c)| {
                    serde_json::json!({
                        "name": name,
                        "desc": c.desc,
                        "aliases": c.aliases,
                        "complete": c.completion.is_some(),
                    })
                })
                .collect();
            list.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
            ret(lua, to_lua(lua, &serde_json::Value::Array(list))?)
        }
        "command_find" => {
            let word: String = args(lua, a)?;
            let found = app
                .user_commands
                .iter()
                .find(|(n, c)| n.as_str() == word || c.aliases.iter().any(|x| x == &word))
                .map(|(n, _)| n.clone());
            ret(lua, found)
        }
        "command_complete" => {
            let (name, ctx): (String, Value) = args(lua, a)?;
            let Some(cb) = app.user_commands.get(&name).and_then(|c| c.completion) else {
                return ret(lua, Value::Nil);
            };
            // Errors are reported like any callback's; the menu shows nothing.
            let r = app.call_callback(cb, "command completion", |_| Ok(ctx));
            ret(lua, r.unwrap_or(Value::Nil))
        }
        "history_add" => {
            let text: String = args(lua, a)?;
            app.history_add(&text);
            ret(lua, ())
        }
        "opt_apply" => {
            let arg: String = args(lua, a)?;
            let r = match apply_dynamic_option(app, &arg) {
                Some(r) => r,
                None => app.options.apply(&arg),
            };
            app.opts_rev += 1;
            app.dirty = true;
            ret(lua, r.map_err(err)?)
        }
        "opt_list" => {
            let mut all: Vec<String> = options::NAMES
                .iter()
                .filter_map(|n| app.options.get(n).map(|v| format!("{n}={v}")))
                .collect();
            let mut dynamic: Vec<String> = app
                .dynamic_options
                .iter()
                .map(|(name, o)| format!("{name}={}", o.value))
                .collect();
            dynamic.sort();
            all.extend(dynamic);
            ret(lua, all)
        }
        "colors_name" => ret(lua, app.colors_name.clone()),
        "log_tail" => {
            let n: usize = args(lua, a)?;
            let start = app.log.len().saturating_sub(n);
            ret(lua, app.log[start..].to_vec())
        }
        "show_message" => {
            // Shown without adding to the log (for /messages itself).
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
        "panel_focused" => ret(lua, app.focused_panel().map(|p| p.id.clone())),
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
        "ui_refresh" => {
            app.views_rev += 1;
            app.ui_broken.clear();
            app.dirty = true;
            ret(lua, ())
        }
        "chat_items" => {
            let opts: Option<Table> = args(lua, a)?;
            let filter = item_filter(&opts)?;
            let items = app.chat_query(&filter);
            // Inside a chat view: what it read decides when it is redrawn.
            if app.render_deps.is_some() {
                let sig = app.chat_signature(&filter);
                if let Some(deps) = &mut app.render_deps {
                    deps.push(crate::data::Dep { filter, sig });
                }
            }
            ret(lua, to_lua(lua, &serde_json::Value::Array(items))?)
        }
        "chat_add" => {
            let (kind, fields, opts): (String, Value, Option<Table>) = args(lua, a)?;
            let builtin = [
                "user",
                "reasoning",
                "assistant",
                "tool",
                "notice",
                "queued",
                "lua",
            ];
            if !valid_dynamic_name(&kind) || builtin.contains(&kind.as_str()) {
                return Err(err(format!(
                    "a chat item kind is lowercase letters, digits and _, not a built-in kind: {kind:?}"
                )));
            }
            let fields = lua_fields(fields)?;
            let buf = match session_opt(&opts)? {
                Some(id) => app
                    .chat_by_session(&id)
                    .ok_or_else(|| err(format!("no open chat for session {id}")))?,
                None => app.current,
            };
            app.lua_item_seq += 1;
            let id = format!("{kind}-{}", app.lua_item_seq);
            app.chats[buf].add_lua(id.clone(), kind, fields);
            app.dirty = true;
            ret(lua, id)
        }
        "chat_update" => {
            let (id, fields): (String, Value) = args(lua, a)?;
            let fields = lua_fields(fields)?;
            let found = app
                .chats
                .iter_mut()
                .any(|c| c.update_lua(&id, fields.clone()));
            app.dirty |= found;
            ret(lua, found)
        }
        "chat_remove" => {
            let id: String = args(lua, a)?;
            let found = app.chats.iter_mut().any(|c| c.remove_lua(&id));
            app.dirty |= found;
            ret(lua, found)
        }
        "spinner_set" => {
            let (frames, interval): (Option<Vec<String>>, Option<f64>) = args(lua, a)?;
            app.spinner = match frames {
                None => Default::default(),
                Some(f) if f.is_empty() => return Err(err("a spinner needs at least one frame")),
                Some(frames) => crate::ui::Spinner {
                    frames,
                    interval_ms: interval.unwrap_or(100.0).max(16.0) as u64,
                },
            };
            app.dirty = true;
            ret(lua, ())
        }
        "chat_at" => {
            let (x, y): (u16, u16) = args(lua, a)?;
            match app.chat_at((x, y)) {
                Some(hit) => ret(lua, to_lua(lua, &hit)?),
                None => ret(lua, Value::Nil),
            }
        }
        "chat_view" => ret(lua, to_lua(lua, &app.chat_view())?),
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
        "chat_refresh_in" => {
            let ms: f64 = args(lua, a)?;
            if app.render_deps.is_none() {
                return Err(err("bone.chat.refresh_in only works inside a chat view"));
            }
            let at =
                std::time::Instant::now() + std::time::Duration::from_millis(ms.max(0.0) as u64);
            app.render_expires = Some(app.render_expires.map_or(at, |e| e.min(at)));
            ret(lua, ())
        }
        "chat_redraw" => {
            let index: Option<usize> = args(lua, a)?;
            let current = app.current;
            app.chats[current].redraw(index);
            app.dirty = true;
            ret(lua, ())
        }
        "chat_turns" => {
            let opts: Option<Table> = args(lua, a)?;
            let session = session_opt(&opts)?;
            // Inside a chat view, turns depend on every item of the chat.
            if app.render_deps.is_some() {
                let filter = crate::data::ItemFilter {
                    session: session.clone(),
                    ..Default::default()
                };
                let sig = app.chat_signature(&filter);
                if let Some(deps) = &mut app.render_deps {
                    deps.push(crate::data::Dep { filter, sig });
                }
            }
            let turns = app.chat_turns(session.as_deref());
            ret(lua, to_lua(lua, &serde_json::Value::Array(turns))?)
        }
        "chat_session" => {
            let opts: Option<Table> = args(lua, a)?;
            ret(
                lua,
                to_lua(lua, &app.chat_session(session_opt(&opts)?.as_deref()))?,
            )
        }
        "chat_sessions" => ret(
            lua,
            to_lua(lua, &serde_json::Value::Array(app.chat_sessions()))?,
        ),
        "prompt_info" => {
            let t = lua.create_table()?;
            t.set("text", app.prompt.text())?;
            t.set("lines", app.prompt.lines().to_vec())?;
            t.set("cursor", pos_out(lua, app.prompt.cursor())?)?;
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

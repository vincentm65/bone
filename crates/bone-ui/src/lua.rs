//! The Lua API ops both frontends share. Each frontend's `bone._api(op, ...)`
//! dispatcher tries [`dispatch`] first and handles the ops it hands back.
//! Ops reach the frontend's state through [`Host`]: accessors that lend its
//! own fields as `bone-ui` types, and the services only it has. See
//! `docs/plans/gui-lua-host.md`.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use bone_lua::{from_lua, to_lua};
use mlua::{FromLua, FromLuaMulti, Function, IntoLuaMulti, Lua, MultiValue, Table, Value};

use crate::chat::{ChatBuffer, find_chat};
use crate::data::{Dep, Indexed, ItemFilter};
use crate::keymap::{Action, Context, Keymaps};
use crate::keys::{self, Key};
use crate::options::{self, DynamicKind, DynamicOption, DynamicValue, Options};
use crate::style::{Color, Modifier};
use crate::theme::{StyleSpec, Theme};

/// The registry table of Lua functions stored by id (keymaps, commands,
/// autocmds, option callbacks...).
pub const CALLBACKS: &str = "bone.callbacks";

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
    /// Its README in `bone._docs`.
    Doc,
}

/// Who owns what: the plugin whose code is running, and what each plugin
/// and stored callback belongs to.
pub struct Owners<'a> {
    /// `None` for the runtime and the user's config.
    pub owner: &'a Option<String>,
    pub callback_owner: &'a mut HashMap<u64, String>,
    pub owned: &'a mut HashMap<String, Vec<Owned>>,
    /// The last id handed out for a stored callback (and autocmds).
    pub next_callback: &'a mut u64,
}

/// The options: the built-in ones, those Lua defined, values kept across a
/// reload for options defined again, and a revision bumped on any change.
pub struct OptionStore<'a> {
    pub options: &'a mut Options,
    pub dynamic: &'a mut HashMap<String, DynamicOption>,
    pub carried: &'a HashMap<String, DynamicValue>,
    pub rev: &'a mut u64,
}

/// The highlight groups, the colorscheme loaded last, and the revision that
/// cached drawing depends on (shared with the options').
pub struct Looks<'a> {
    pub theme: &'a mut Theme,
    pub colors_name: &'a mut Option<String>,
    pub rev: &'a mut u64,
}

/// The open chats, which one is on screen, and while a chat view draws,
/// what it read (`render_deps`) and when it asked to be drawn again.
pub struct ChatStore<'a, L> {
    pub chats: &'a mut Vec<ChatBuffer<L>>,
    pub current: usize,
    /// The chat a view is drawing, which queries default to.
    pub rendering: Option<usize>,
    pub render_deps: &'a mut Option<Vec<Dep>>,
    pub render_expires: &'a mut Option<Instant>,
    /// Numbers the items Lua adds to chats.
    pub lua_item_seq: &'a mut u64,
}

impl<L> ChatStore<'_, L> {
    /// The chat a query is about: by session id, else the one being drawn
    /// or on screen. `None` for a session no open chat has.
    pub fn data_chat(&self, session: Option<&str>) -> Option<&ChatBuffer<L>> {
        let buf = match session {
            Some(id) => find_chat(self.chats, id)?,
            None => self.rendering.unwrap_or(self.current),
        };
        self.chats.get(buf)
    }

    /// See [`Indexed::signature`]; 0 for a chat that isn't open.
    pub fn signature(&self, f: &ItemFilter) -> u64 {
        self.data_chat(f.session.as_deref())
            .map_or(0, |c| Indexed::new(c).signature(f))
    }

    /// Inside a chat view, remember that it read what `filter` matches.
    fn read(&mut self, filter: ItemFilter) {
        if self.render_deps.is_some() {
            let sig = self.signature(&filter);
            if let Some(deps) = self.render_deps {
                deps.push(Dep { filter, sig });
            }
        }
    }
}

/// The keymaps, and which plugin last mapped each key.
pub struct KeymapStore<'a> {
    pub keymaps: &'a mut Keymaps,
    pub owner: &'a mut HashMap<(Context, Vec<Key>), String>,
}

/// A slash command defined from Lua (`bone.cmd.create`).
#[derive(Clone)]
pub struct UserCommand {
    pub callback: u64,
    pub desc: String,
    pub aliases: Vec<String>,
    pub completion: Option<u64>,
    pub args: Option<serde_json::Value>,
}

/// An event handler (`bone.on`).
pub struct Autocmd {
    pub id: u64,
    pub event: String,
    pub callback: u64,
}

/// The one name of an event that has aliases.
pub fn canonical_event(name: &str) -> &str {
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

/// What shared ops need from a frontend.
pub trait Host {
    /// The frontend's rendered line type, cached in its chats.
    type Line;
    fn chat_store(&mut self) -> ChatStore<'_, Self::Line>;
    fn owners(&mut self) -> Owners<'_>;
    fn option_store(&mut self) -> OptionStore<'_>;
    fn autocmds(&mut self) -> &mut Vec<Autocmd>;
    fn keymap_store(&mut self) -> KeymapStore<'_>;
    fn looks(&mut self) -> Looks<'_>;
    /// Where the user's config, plugins and runtime overrides are.
    fn config_dir(&self) -> Option<&Path>;
    /// Run Lua that may call the API again, from inside an op: the API is
    /// bound afresh for it, as for the frontend's own Lua calls.
    fn with_api<R>(&mut self, body: impl FnOnce(&Lua) -> mlua::Result<R>) -> mlua::Result<R>;
    /// The keymap context that has the keyboard: a focused popup or panel's,
    /// a focused named context, or `main`.
    fn context(&self) -> Context;
    fn user_commands(&mut self) -> &mut HashMap<String, UserCommand>;
    /// Run stored callback `id` with one argument; as `call_callback_multi`.
    fn call_callback(
        &mut self,
        id: u64,
        context: &str,
        arg: impl FnOnce(&Lua) -> mlua::Result<Value>,
    ) -> Option<Value>;
    /// Run stored callback `id` with `args`, as the plugin that owns it; a
    /// failure is reported as `context` and gives `None`.
    fn call_callback_multi(
        &mut self,
        id: u64,
        context: &str,
        args: impl FnOnce(&Lua) -> mlua::Result<MultiValue>,
    ) -> Option<MultiValue>;
    /// Something on screen may have changed.
    fn changed(&mut self);
}

/// Run shared op `op`, or give `a` back for the frontend's own dispatcher.
pub fn dispatch<H: Host>(
    h: &mut H,
    lua: &Lua,
    op: &str,
    a: MultiValue,
) -> Result<mlua::Result<MultiValue>, MultiValue> {
    Ok(match op {
        "opt_get" => opt_get(h, lua, a),
        "opt_set" => opt_set(h, lua, a),
        "opt_define" => opt_define(h, lua, a),
        "opt_del" => opt_del(h, lua, a),
        "opt_names" => opt_names(h, lua),
        "opt_info" => opt_info(h, lua, a),
        "opt_apply" => opt_apply(h, lua, a),
        "opt_list" => opt_list(h, lua),
        "keymap_set" => keymap_set(h, lua, a),
        "keymap_del" => keymap_del(h, lua, a),
        "keymap_context" => keymap_context(h, lua, a),
        "keymap_current" => ret(lua, h.context().name().to_owned()),
        "keymap_list" => keymap_list(h, lua),
        "hl_set" => hl_set(h, lua, a),
        "hl_get" => hl_get(h, lua, a),
        "hl_reset" => {
            let l = h.looks();
            l.theme.reset();
            *l.rev += 1;
            h.changed();
            ret(lua, ())
        }
        "hl_names" => {
            let names: Vec<String> = h
                .looks()
                .theme
                .names()
                .into_iter()
                .map(str::to_owned)
                .collect();
            ret(lua, names)
        }
        "colorscheme" => colorscheme_op(h, lua, a),
        "colors_name" => ret(lua, h.looks().colors_name.clone()),
        "chat_items" => chat_items(h, lua, a),
        "chat_add" => chat_add(h, lua, a),
        "chat_update" => chat_update(h, lua, a),
        "chat_remove" => chat_remove(h, lua, a),
        "chat_refresh_in" => chat_refresh_in(h, lua, a),
        "chat_redraw" => chat_redraw(h, lua, a),
        "chat_turns" => chat_turns(h, lua, a),
        "chat_session" => chat_session(h, lua, a),
        "chat_sessions" => chat_sessions(h, lua),
        "on" => on(h, lua, a),
        "off" => off(h, lua, a),
        "command_create" => command_create(h, lua, a),
        "command_del" => command_del(h, lua, a),
        "command_list" => command_list(h, lua),
        "command_find" => command_find(h, lua, a),
        "command_complete" => command_complete(h, lua, a),
        _ => return Err(a),
    })
}

// ---- helpers ---------------------------------------------------------------

pub fn args<T: FromLuaMulti>(lua: &Lua, args: MultiValue) -> mlua::Result<T> {
    T::from_lua_multi(args, lua)
}

pub fn ret(lua: &Lua, v: impl IntoLuaMulti) -> mlua::Result<MultiValue> {
    v.into_lua_multi(lua)
}

pub fn err(msg: impl Into<String>) -> mlua::Error {
    mlua::Error::runtime(msg.into())
}

/// `opts[k]`, for an optional options table.
pub fn opt<T: FromLua>(opts: &Option<Table>, k: &str) -> mlua::Result<Option<T>> {
    match opts {
        Some(o) => o.get(k),
        None => Ok(None),
    }
}

/// Keep `f` for later calls by id; it belongs to the plugin running now.
pub fn store_callback<H: Host>(h: &mut H, lua: &Lua, f: Function) -> mlua::Result<u64> {
    let o = h.owners();
    *o.next_callback += 1;
    let id = *o.next_callback;
    lua.named_registry_value::<Table>(CALLBACKS)?.set(id, f)?;
    if let Some(owner) = o.owner {
        o.callback_owner.insert(id, owner.clone());
    }
    Ok(id)
}

pub fn drop_callback(lua: &Lua, id: u64) -> mlua::Result<()> {
    lua.named_registry_value::<Table>(CALLBACKS)?
        .set(id, Value::Nil)
}

/// Record that the plugin running now created `what`.
pub fn own<H: Host>(h: &mut H, what: Owned) {
    let o = h.owners();
    if let Some(owner) = o.owner {
        o.owned.entry(owner.clone()).or_default().push(what);
    }
}

/// `opts.context`: a keymap context name (default `main`).
pub fn context_arg(opts: &Option<Table>) -> mlua::Result<Context> {
    match opt::<String>(opts, "context")? {
        None => Ok(Context::Main),
        Some(c) => Context::from_name(&c).ok_or_else(|| {
            mlua::Error::runtime(format!(
                "invalid context {c:?} (use main, popup, or letters, digits, _, - and .)"
            ))
        }),
    }
}

pub fn context_from_name(name: String) -> mlua::Result<Context> {
    Context::from_name(&name)
        .ok_or_else(|| err(format!("invalid context {name:?} (bad context name)")))
}

/// `opts.fallback` (or `fallbacks`): a context name or a list of them.
pub fn fallback_arg(opts: &Option<Table>) -> mlua::Result<Option<Vec<Context>>> {
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

/// Lowercase letters, digits and `_`, starting with a letter.
pub fn valid_dynamic_name(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

// ---- options ---------------------------------------------------------------

pub fn dynamic_from_lua(
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

pub fn dynamic_to_lua(lua: &Lua, value: &DynamicValue) -> mlua::Result<Value> {
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

fn dynamic_option_arg<'a>(
    dynamic: &HashMap<String, DynamicOption>,
    arg: &'a str,
) -> Option<DynamicOptionArg<'a>> {
    if let Some(name) = arg.strip_suffix('?')
        && dynamic.contains_key(name)
    {
        return Some(DynamicOptionArg::Query(name));
    }
    if let Some((name, value)) = arg.split_once('=')
        && dynamic.contains_key(name)
    {
        return Some(DynamicOptionArg::Set(name, value));
    }
    if dynamic.contains_key(arg) {
        return Some(DynamicOptionArg::Enable(arg));
    }
    if let Some(name) = arg.strip_prefix("no")
        && dynamic.contains_key(name)
    {
        return Some(DynamicOptionArg::Disable(name));
    }
    if let Some(name) = arg.strip_prefix("inv")
        && dynamic.contains_key(name)
    {
        return Some(DynamicOptionArg::Toggle(name));
    }
    if let Some(name) = arg.strip_suffix('!')
        && dynamic.contains_key(name)
    {
        return Some(DynamicOptionArg::Toggle(name));
    }
    None
}

/// Apply one option argument (`bone.o.apply`) to a dynamic option. `None`
/// means the argument belongs to the fixed Rust options instead.
pub fn apply_dynamic_option<H: Host>(
    h: &mut H,
    arg: &str,
) -> Option<Result<Option<String>, String>> {
    let operation = dynamic_option_arg(h.option_store().dynamic, arg)?;
    Some((|| {
        let (name, kind, current) = match &operation {
            DynamicOptionArg::Query(name)
            | DynamicOptionArg::Set(name, _)
            | DynamicOptionArg::Enable(name)
            | DynamicOptionArg::Disable(name)
            | DynamicOptionArg::Toggle(name) => {
                let s = h.option_store();
                let option = s.dynamic.get(*name).expect("dynamic option found");
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
        let s = h.option_store();
        options::check_choice(&name, &s.dynamic[name.as_str()], &new)?;
        if current == new {
            return Ok(None);
        }
        let callback = {
            let option = s
                .dynamic
                .get_mut(name.as_str())
                .expect("dynamic option found");
            option.value = new.clone();
            option.on_change
        };
        if let Some(callback) = callback {
            h.call_callback_multi(callback, &format!("option {name}"), |lua| {
                Ok(vec![dynamic_to_lua(lua, &new)?, dynamic_to_lua(lua, &current)?].into())
            });
        }
        Ok(None)
    })())
}

fn opt_get<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let name: String = args(lua, a)?;
    let s = h.option_store();
    if let Some(value) = s.options.get(&name) {
        return match value {
            options::Value::Bool(b) => ret(lua, b),
            options::Value::Number(n) => ret(lua, n),
        };
    }
    let value = s
        .dynamic
        .get(&name)
        .ok_or_else(|| err(format!("unknown option: {name}")))?
        .value
        .clone();
    ret(lua, dynamic_to_lua(lua, &value)?)
}

fn opt_set<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let (name, value): (String, Value) = args(lua, a)?;
    let s = h.option_store();
    if s.options.get(&name).is_some() {
        let value = match value {
            Value::Boolean(b) => options::Value::Bool(b),
            Value::Integer(i) if i >= 0 => options::Value::Number(i as u64),
            Value::Number(n) if n >= 0.0 && n.fract() == 0.0 => options::Value::Number(n as u64),
            other => {
                return Err(err(format!("bad value for {name}: {}", other.type_name())));
            }
        };
        s.options.set(&name, value).map_err(err)?;
    } else {
        let kind = s
            .dynamic
            .get(&name)
            .ok_or_else(|| err(format!("unknown option: {name}")))?
            .kind;
        let (_, new_value) = dynamic_from_lua(value, Some(kind))?;
        options::check_choice(&name, &s.dynamic[&name], &new_value).map_err(err)?;
        let (old_value, callback) = {
            let option = s.dynamic.get_mut(&name).expect("option found");
            let old = option.value.clone();
            option.value = new_value.clone();
            (old, option.on_change)
        };
        if old_value != new_value
            && let Some(callback) = callback
        {
            let new_value = new_value.clone();
            h.call_callback_multi(callback, &format!("option {name}"), |lua| {
                Ok(vec![
                    dynamic_to_lua(lua, &new_value)?,
                    dynamic_to_lua(lua, &old_value)?,
                ]
                .into())
            });
        }
    }
    *h.option_store().rev += 1;
    h.changed();
    ret(lua, ())
}

fn opt_define<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let (name, default, opts): (String, Value, Option<Table>) = args(lua, a)?;
    if !valid_dynamic_name(&name) {
        return Err(err(format!("invalid option name {name:?}")));
    }
    if options::NAMES.contains(&name.as_str()) {
        return Err(err(format!("{name} is a built-in option")));
    }
    if h.option_store().dynamic.contains_key(&name) {
        return Err(err(format!("option already defined: {name}")));
    }
    // After a reload, an option defined again keeps its value.
    let previous = h.option_store().carried.get(&name).cloned();
    let requested = opt::<String>(&opts, "type")?
        .map(|name| {
            DynamicKind::parse(&name).ok_or_else(|| err(format!("unknown option type {name:?}")))
        })
        .transpose()?;
    let (kind, value) = dynamic_from_lua(default, requested)?;
    let desc = opt::<String>(&opts, "desc")?.unwrap_or_default();
    let callback = opt::<Function>(&opts, "on_change")?
        .map(|f| store_callback(h, lua, f))
        .transpose()?;
    let current = previous
        .filter(|v| v.kind() == kind)
        .unwrap_or_else(|| value.clone());
    let choices: Vec<String> = opt::<Vec<String>>(&opts, "choices")?.unwrap_or_default();
    if !choices.is_empty() {
        let DynamicValue::String(d) = &value else {
            return Err(err(format!("{name}: choices are for string options")));
        };
        if !choices.contains(d) {
            return Err(err(format!(
                "{name}: the default {d:?} is not one of its choices"
            )));
        }
    }
    let current = match &current {
        DynamicValue::String(s) if !choices.is_empty() && !choices.contains(s) => value.clone(),
        _ => current,
    };
    own(h, Owned::Option(name.clone()));
    let owner = h.owners().owner.clone();
    let s = h.option_store();
    s.dynamic.insert(
        name,
        DynamicOption {
            value: current,
            default: value,
            kind,
            desc,
            choices,
            on_change: callback,
            owner,
        },
    );
    *s.rev += 1;
    h.changed();
    ret(lua, ())
}

fn opt_del<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let name: String = args(lua, a)?;
    let s = h.option_store();
    let Some(option) = s.dynamic.remove(&name) else {
        return ret(lua, false);
    };
    if let Some(callback) = option.on_change {
        drop_callback(lua, callback)?;
    }
    *s.rev += 1;
    h.changed();
    ret(lua, true)
}

fn opt_names<H: Host>(h: &mut H, lua: &Lua) -> mlua::Result<MultiValue> {
    let mut names: Vec<String> = options::NAMES
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    names.extend(h.option_store().dynamic.keys().cloned());
    names.sort();
    ret(lua, names)
}

fn opt_info<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let name: String = args(lua, a)?;
    let table = lua.create_table()?;
    let s = h.option_store();
    if let Some(value) = s.options.get(&name) {
        let (kind, value) = match value {
            options::Value::Bool(value) => ("boolean", Value::Boolean(value)),
            options::Value::Number(value) => ("integer", Value::Integer(value as i64)),
        };
        table.set("type", kind)?;
        table.set("value", value)?;
        table.set("desc", options::describe(&name))?;
        match options::Options::default().get(&name) {
            Some(options::Value::Bool(d)) => table.set("default", d)?,
            Some(options::Value::Number(d)) => table.set("default", d as i64)?,
            None => {}
        }
    } else if let Some(option) = s.dynamic.get(&name) {
        table.set("type", option.kind.name())?;
        table.set("value", dynamic_to_lua(lua, &option.value)?)?;
        table.set("default", dynamic_to_lua(lua, &option.default)?)?;
        table.set("desc", option.desc.as_str())?;
        if !option.choices.is_empty() {
            table.set("choices", option.choices.clone())?;
        }
    } else {
        return Err(err(format!("unknown option: {name}")));
    }
    ret(lua, table)
}

fn opt_apply<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let arg: String = args(lua, a)?;
    let r = match apply_dynamic_option(h, &arg) {
        Some(r) => r,
        None => h.option_store().options.apply(&arg),
    };
    *h.option_store().rev += 1;
    h.changed();
    ret(lua, r.map_err(err)?)
}

fn opt_list<H: Host>(h: &mut H, lua: &Lua) -> mlua::Result<MultiValue> {
    let s = h.option_store();
    let mut all: Vec<String> = options::NAMES
        .iter()
        .filter_map(|n| s.options.get(n).map(|v| format!("{n}={v}")))
        .collect();
    let mut dynamic: Vec<String> = s
        .dynamic
        .iter()
        .map(|(name, o)| format!("{name}={}", o.value))
        .collect();
    dynamic.sort();
    all.extend(dynamic);
    ret(lua, all)
}

// ---- events ----------------------------------------------------------------

/// Run the handlers of `event` with `data`; what they returned.
pub fn fire<H: Host>(h: &mut H, event: &str, data: serde_json::Value) -> Vec<Value> {
    let event = canonical_event(event);
    let cbs: Vec<u64> = h
        .autocmds()
        .iter()
        .filter(|a| a.event == event)
        .map(|a| a.callback)
        .collect();
    let mut out = Vec::new();
    for cb in cbs {
        let data = data.clone();
        if let Some(v) = h.call_callback(cb, &format!("autocmd {event}"), |lua| to_lua(lua, &data))
        {
            out.push(v);
        }
    }
    out
}

pub fn has_autocmd<H: Host>(h: &mut H, event: &str) -> bool {
    let event = canonical_event(event);
    h.autocmds().iter().any(|a| a.event == event)
}

fn on<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let (event, f): (String, Function) = args(lua, a)?;
    let callback = store_callback(h, lua, f)?;
    let next = h.owners().next_callback;
    *next += 1;
    let id = *next;
    let event = canonical_event(&event).to_owned();
    h.autocmds().push(Autocmd {
        id,
        event,
        callback,
    });
    own(h, Owned::Autocmd(id));
    ret(lua, id)
}

fn off<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let id: u64 = args(lua, a)?;
    let autocmds = h.autocmds();
    if let Some(i) = autocmds.iter().position(|a| a.id == id) {
        let a = autocmds.remove(i);
        drop_callback(lua, a.callback)?;
    }
    ret(lua, ())
}

// ---- commands --------------------------------------------------------------

fn command_create<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
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
    let aliases = match opt::<Value>(&opts, "aliases")? {
        None => Vec::new(),
        Some(Value::String(alias)) => vec![alias.to_str()?.to_owned()],
        Some(Value::Table(aliases)) => aliases
            .sequence_values::<String>()
            .collect::<mlua::Result<Vec<_>>>()?,
        Some(other) => {
            return Err(err(format!(
                "command aliases must be a string or list, not {}",
                other.type_name()
            )));
        }
    };
    let commands = h.user_commands();
    if commands
        .iter()
        .any(|(old_name, old)| old_name != &name && old.aliases.iter().any(|alias| alias == &name))
    {
        return Err(err(format!("/{name} is already a user command")));
    }
    for (index, alias) in aliases.iter().enumerate() {
        // Any word without spaces or "/" (so "?" can be /help's).
        let valid =
            !alias.is_empty() && !alias.contains(char::is_whitespace) && !alias.contains('/');
        if !valid {
            return Err(err(format!("invalid command alias {alias:?}")));
        }
        if alias == &name || aliases[..index].contains(alias) {
            return Err(err(format!("/{alias} is already a command")));
        }
        if commands.iter().any(|(old_name, old)| {
            old_name != &name && (old_name == alias || old.aliases.iter().any(|a| a == alias))
        }) {
            return Err(err(format!("/{alias} is already a user command")));
        }
    }
    let args_spec = match opt::<Value>(&opts, "args")? {
        None => None,
        Some(Value::Table(spec)) => {
            let value = from_lua(&Value::Table(spec))?;
            crate::commands::validate_argument_specs(&value).map_err(err)?;
            Some(value)
        }
        Some(other) => {
            return Err(err(format!(
                "command args must be a table, not {}",
                other.type_name()
            )));
        }
    };
    let desc = opt::<String>(&opts, "desc")?.unwrap_or_default();
    let completion = match opt::<Function>(&opts, "complete")? {
        Some(f) => Some(f),
        None => opt(&opts, "completion")?,
    }
    .map(|f| store_callback(h, lua, f))
    .transpose()?;
    let callback = store_callback(h, lua, f)?;
    own(h, Owned::Command(name.clone(), callback));
    if let Some(old) = h.user_commands().insert(
        name,
        UserCommand {
            callback,
            desc,
            aliases,
            completion,
            args: args_spec,
        },
    ) {
        drop_callback(lua, old.callback)?;
        if let Some(completion) = old.completion {
            drop_callback(lua, completion)?;
        }
    }
    ret(lua, ())
}

fn command_del<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let name: String = args(lua, a)?;
    let commands = h.user_commands();
    let canonical = commands
        .iter()
        .find(|(canonical, command)| {
            canonical.as_str() == name || command.aliases.iter().any(|alias| alias == &name)
        })
        .map(|(canonical, _)| canonical.clone())
        .ok_or_else(|| err(format!("no user command {name}")))?;
    let old = commands.remove(&canonical).expect("command found");
    drop_callback(lua, old.callback)?;
    if let Some(completion) = old.completion {
        drop_callback(lua, completion)?;
    }
    ret(lua, ())
}

fn command_list<H: Host>(h: &mut H, lua: &Lua) -> mlua::Result<MultiValue> {
    let mut list: Vec<serde_json::Value> = h
        .user_commands()
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

fn command_find<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let word: String = args(lua, a)?;
    let found = h
        .user_commands()
        .iter()
        .find(|(n, c)| n.as_str() == word || c.aliases.iter().any(|x| x == &word))
        .map(|(n, _)| n.clone());
    ret(lua, found)
}

fn command_complete<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let (name, ctx): (String, Value) = args(lua, a)?;
    let Some(cb) = h.user_commands().get(&name).and_then(|c| c.completion) else {
        return ret(lua, Value::Nil);
    };
    // Errors are reported like any callback's; the menu shows nothing.
    let r = h.call_callback(cb, "command completion", |_| Ok(ctx));
    ret(lua, r.unwrap_or(Value::Nil))
}

// ---- keymaps ---------------------------------------------------------------

fn keymap_set<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let (key, rhs, opts): (String, Value, Option<Table>) = args(lua, a)?;
    let ctx = context_arg(&opts)?;
    let desc: Option<String> = opt(&opts, "desc")?;
    let action = match rhs {
        Value::String(s) => Action::parse(&s.to_str()?).map_err(err)?,
        Value::Function(f) => Action::Lua(store_callback(h, lua, f)?),
        other => {
            return Err(err(format!(
                "keymap action must be a string or function, not {}",
                other.type_name()
            )));
        }
    };
    let seq = keys::parse_sequence(&key).map_err(err)?;
    let owner = h.owners().owner.clone();
    let k = h.keymap_store();
    k.keymaps
        .set(ctx.clone(), &key, action, desc)
        .map_err(err)?;
    match owner {
        Some(owner) => {
            k.owner.insert((ctx, seq), owner);
        }
        None => {
            k.owner.remove(&(ctx, seq));
        }
    }
    ret(lua, ())
}

fn keymap_del<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let (key, opts): (String, Option<Table>) = args(lua, a)?;
    let ctx = context_arg(&opts)?;
    let k = h.keymap_store();
    k.keymaps.del(ctx.clone(), &key).map_err(err)?;
    k.owner
        .remove(&(ctx, keys::parse_sequence(&key).map_err(err)?));
    ret(lua, ())
}

fn keymap_context<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let (name, opts): (String, Option<Table>) = args(lua, a)?;
    let ctx = context_from_name(name)?;
    if !h.keymap_store().keymaps.has_context(&ctx) {
        own(h, Owned::Context(ctx.clone()));
    }
    let fallback = fallback_arg(&opts)?;
    let priority = opt::<i32>(&opts, "priority")?;
    h.keymap_store()
        .keymaps
        .define(ctx, fallback, priority)
        .map_err(err)?;
    ret(lua, ())
}

fn keymap_list<H: Host>(h: &mut H, lua: &Lua) -> mlua::Result<MultiValue> {
    let k = h.keymap_store();
    let list: Vec<_> = (k.keymaps.described())
        .map(|(ctx, seq, desc)| {
            let keys: Vec<_> = seq.iter().map(keys::format).collect();
            let owner = k.owner.get(&(ctx.clone(), seq.to_vec()));
            serde_json::json!({
                "keys": keys.join(" "), "desc": desc,
                "context": ctx.name(), "plugin": owner,
            })
        })
        .collect();
    ret(lua, to_lua(lua, &serde_json::Value::Array(list))?)
}

// ---- highlights ------------------------------------------------------------

fn hl_set<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let (name, spec): (String, Table) = args(lua, a)?;
    let spec = StyleSpec {
        fg: spec.get("fg")?,
        bg: spec.get("bg")?,
        bold: spec.get::<Option<bool>>("bold")?.unwrap_or(false),
        italic: spec.get::<Option<bool>>("italic")?.unwrap_or(false),
        underline: spec.get::<Option<bool>>("underline")?.unwrap_or(false),
        strikethrough: spec.get::<Option<bool>>("strikethrough")?.unwrap_or(false),
        reverse: spec.get::<Option<bool>>("reverse")?.unwrap_or(false),
        dim: spec.get::<Option<bool>>("dim")?.unwrap_or(false),
        link: spec.get("link")?,
    };
    let l = h.looks();
    l.theme
        .set(&name, &spec)
        .map_err(|e| err(format!("{name}: {e}")))?;
    *l.rev += 1;
    h.changed();
    ret(lua, ())
}

/// A color as `parse_color` reads it back.
fn fmt_color(c: Color) -> String {
    match c {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Indexed(n) => n.to_string(),
        other => format!("{other:?}").to_ascii_lowercase(),
    }
}

fn hl_get<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let name: String = args(lua, a)?;
    let st = h.looks().theme.hl(&name);
    let t = lua.create_table()?;
    t.set("fg", st.fg.map(fmt_color))?;
    t.set("bg", st.bg.map(fmt_color))?;
    for (key, m) in [
        ("bold", Modifier::BOLD),
        ("italic", Modifier::ITALIC),
        ("underline", Modifier::UNDERLINED),
        ("strikethrough", Modifier::CROSSED_OUT),
        ("reverse", Modifier::REVERSED),
        ("dim", Modifier::DIM),
    ] {
        if st.add_modifier.contains(m) {
            t.set(key, true)?;
        }
    }
    ret(lua, t)
}

fn colorscheme_op<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let name: String = args(lua, a)?;
    colorscheme(h, &name).map_err(err)?;
    ret(lua, ())
}

/// Load `colors/<name>.lua`: the user's, then a plugin's, then the
/// library's or built-in one.
pub fn colorscheme<H: Host>(h: &mut H, name: &str) -> Result<(), String> {
    if name.is_empty() || name.contains(['/', '\\', '.']) {
        return Err(format!("bad colorscheme name {name:?}"));
    }
    let rel = format!("colors/{name}.lua");
    let dir = h.config_dir().map(Path::to_owned);
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
        .or_else(|| bone_lua::tui_runtime_source(dir.as_deref(), &rel))
        .ok_or_else(|| format!("no colorscheme named {name}"))?;
    // Its `bone.hl.set` calls replace the previous colorscheme's groups;
    // the UI's, plugins' and the user's stay. One loaded from inside
    // another colorscheme adds to it.
    let saved = h.looks().theme.begin_scheme();
    let outer = saved.is_some();
    let loaded = h.with_api(|lua| lua.load(src).set_name(format!("@{chunk}")).exec());
    h.looks().theme.end_scheme(saved, loaded.is_ok());
    loaded.map_err(|e| format!("colorscheme {name}: {}", bone_lua::short_error(&e)))?;
    let l = h.looks();
    if outer {
        *l.colors_name = Some(name.to_owned());
    }
    *l.rev += 1;
    h.changed();
    Ok(())
}

// ---- chat data -------------------------------------------------------------

/// `bone.chat.items` options as a filter.
pub fn item_filter(opts: &Option<Table>) -> mlua::Result<ItemFilter> {
    let Some(o) = opts else {
        return Ok(Default::default());
    };
    let kind = match o.get::<Value>("kind")? {
        Value::Nil => None,
        Value::String(s) => Some(vec![s.to_str()?.to_owned()]),
        Value::Table(t) => Some(t.sequence_values::<String>().collect::<mlua::Result<_>>()?),
        other => {
            return Err(err(format!(
                "kind is a string or a list of them, not {}",
                other.type_name()
            )));
        }
    };
    Ok(ItemFilter {
        kind,
        around: o.get("around")?,
        name: o.get("name")?,
        turn: o.get("turn")?,
        running: o.get("running")?,
        error: o.get("error")?,
        from: o.get("from")?,
        to: o.get("to")?,
        first: o.get("first")?,
        last: o.get("last")?,
        session: o.get("session")?,
        full: o.get::<Option<bool>>("full")?.unwrap_or(false),
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

fn chat_items<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let opts: Option<Table> = args(lua, a)?;
    let filter = item_filter(&opts)?;
    let mut c = h.chat_store();
    let items = c
        .data_chat(filter.session.as_deref())
        .map_or_else(Vec::new, |chat| Indexed::new(chat).query(&filter));
    // Inside a chat view: what it read decides when it is redrawn.
    c.read(filter);
    ret(lua, to_lua(lua, &serde_json::Value::Array(items))?)
}

fn chat_add<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
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
    let c = h.chat_store();
    let buf = match opt::<String>(&opts, "session")? {
        Some(id) => {
            find_chat(c.chats, &id).ok_or_else(|| err(format!("no open chat for session {id}")))?
        }
        None => c.current,
    };
    *c.lua_item_seq += 1;
    let id = format!("{kind}-{}", c.lua_item_seq);
    c.chats[buf].add_lua(id.clone(), kind, fields);
    h.changed();
    ret(lua, id)
}

fn chat_update<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let (id, fields): (String, Value) = args(lua, a)?;
    let fields = lua_fields(fields)?;
    let found = h
        .chat_store()
        .chats
        .iter_mut()
        .any(|c| c.update_lua(&id, fields.clone()));
    if found {
        h.changed();
    }
    ret(lua, found)
}

fn chat_remove<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let id: String = args(lua, a)?;
    let found = h.chat_store().chats.iter_mut().any(|c| c.remove_lua(&id));
    if found {
        h.changed();
    }
    ret(lua, found)
}

fn chat_refresh_in<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let ms: f64 = args(lua, a)?;
    let c = h.chat_store();
    if c.render_deps.is_none() {
        return Err(err("bone.chat.refresh_in only works inside a chat view"));
    }
    let at = Instant::now() + Duration::from_millis(ms.max(0.0) as u64);
    *c.render_expires = Some(c.render_expires.map_or(at, |e| e.min(at)));
    ret(lua, ())
}

fn chat_redraw<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let index: Option<usize> = args(lua, a)?;
    let c = h.chat_store();
    let current = c.current;
    c.chats[current].redraw(index);
    h.changed();
    ret(lua, ())
}

fn chat_turns<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let opts: Option<Table> = args(lua, a)?;
    let session = opt::<String>(&opts, "session")?;
    let mut c = h.chat_store();
    // Inside a chat view, turns depend on every item of the chat.
    c.read(ItemFilter {
        session: session.clone(),
        ..Default::default()
    });
    let turns = c
        .data_chat(session.as_deref())
        .map_or_else(Vec::new, crate::data::turns);
    ret(lua, to_lua(lua, &serde_json::Value::Array(turns))?)
}

fn chat_session<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let opts: Option<Table> = args(lua, a)?;
    let session = opt::<String>(&opts, "session")?;
    let c = h.chat_store();
    let v = match c.data_chat(session.as_deref()) {
        Some(chat) => {
            let buf = (c.chats.iter()).position(|x| std::ptr::eq(x, chat));
            crate::data::session(chat, buf, buf == Some(c.current))
        }
        None => serde_json::Value::Null,
    };
    ret(lua, to_lua(lua, &v)?)
}

fn chat_sessions<H: Host>(h: &mut H, lua: &Lua) -> mlua::Result<MultiValue> {
    let c = h.chat_store();
    let all = crate::data::sessions(c.chats, c.current);
    ret(lua, to_lua(lua, &serde_json::Value::Array(all))?)
}

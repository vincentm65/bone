//! The Lua API ops both frontends share. Each frontend's `bone._api(op, ...)`
//! dispatcher tries [`dispatch`] first and handles the ops it hands back.
//! Ops reach the frontend's state through [`Host`]: accessors that lend its
//! own fields as `bone-ui` types, and the services only it has. See
//! `docs/plans/gui-lua-host.md`.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::Path;
use std::time::{Duration, Instant};

use bone_lua::{from_lua, to_lua};
use bone_proto::methods::ProcessSnapshot;
use bone_proto::types::ImageAttachment;
use mlua::{FromLua, FromLuaMulti, Function, IntoLuaMulti, Lua, MultiValue, Table, Value};

pub mod input;
pub mod jobs;
pub mod libraries;
pub mod panels;
pub mod plugins;
pub mod processes;
pub mod state;
pub mod ui;

use crate::chat::{ChatBuffer, Item as ChatItem, RenderKey, find_chat};
use crate::data::{Dep, Indexed, ItemFilter};
use crate::keymap::{Action, Builtin, Context, Keymaps};
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
    pub owner: &'a mut Option<String>,
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

/// The last frame's layout, for `bone.ui.geometry` and pointer hits.
#[derive(Debug, Default, Clone)]
pub struct Geometry {
    pub leaves: Vec<(String, crate::layout::Rect)>,
    pub seps: Vec<(crate::layout::Rect, Vec<usize>)>,
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
    /// Run `f` as the plugin that owns callback `cb` (or as the user's
    /// config), so what it creates belongs to that plugin.
    fn as_owner_of<R>(&mut self, cb: u64, f: impl FnOnce(&mut Self) -> R) -> R
    where
        Self: Sized,
    {
        let o = self.owners();
        let owner = o.callback_owner.get(&cb).cloned();
        let prev = std::mem::replace(o.owner, owner);
        let r = f(self);
        *self.owners().owner = prev;
        r
    }

    // Services.
    /// Send a core request; `then` runs on the UI thread with the reply.
    fn request_raw(
        &mut self,
        method: String,
        params: serde_json::Value,
        then: impl FnOnce(&mut Self, Result<serde_json::Value, String>) + Send + 'static,
    ) where
        Self: Sized;
    /// Run `fut` in the background; `then` gets its output on the UI thread.
    fn spawn<T: Send + 'static>(
        &mut self,
        fut: impl Future<Output = T> + Send + 'static,
        then: impl FnOnce(&mut Self, T) + Send + 'static,
    ) where
        Self: Sized;
    /// A plugin is being loaded or unloaded through the core: its files
    /// change on purpose (the TUI's watcher takes that as no edit).
    fn plugin_files_changing(&mut self) {}
    fn info(&mut self, text: String);
    fn error(&mut self, text: String);
    /// Report a Lua error from `context`.
    fn lua_error(&mut self, context: &str, e: &mlua::Error);
    /// The saved settings, as the core last sent them.
    fn settings(&self) -> &serde_json::Value;
    /// The chat `bone.session` is about.
    fn target_chat(&self) -> Option<usize>;
    /// The prompt's text, pastes expanded.
    fn prompt_text(&self) -> String;
    fn prompt_images(&self) -> Vec<ImageAttachment>;
    fn set_prompt_images(&mut self, images: Vec<ImageAttachment>) -> Result<(), String>;
    /// Replace the prompt's text.
    fn set_prompt_text(&mut self, text: &str);
    /// `bone.prompt.info()`: text, images, lines, cursor, selection, and
    /// whether history is being browsed.
    fn prompt_info(&mut self, lua: &Lua) -> mlua::Result<Table>;
    /// The prompt has no text and no images.
    fn prompt_empty(&self) -> bool;
    /// Remember a sent prompt or command for recall.
    fn history_add(&mut self, text: &str);
    /// The id of the panel with the keyboard.
    fn focused_panel(&self) -> Option<String>;
    /// The managed shell processes of the session on screen.
    fn process_list(&self) -> Vec<ProcessSnapshot>;
    /// What is kept of them (see `processes`).
    fn processes(&mut self) -> &mut processes::Processes;
    /// Where the chat is scrolled, in the frontend's rows (`bone.chat.view`).
    fn chat_view(&self) -> serde_json::Value;
    /// `bone.ui` changed: draw it all again, retrying parts that failed.
    fn refresh_ui(&mut self);
    /// Put `text` on the system clipboard.
    fn clipboard_copy(&mut self, text: String);
    /// This frontend's checks for `/health`: `{ name, status, message }`.
    fn frontend_health(&self) -> Vec<serde_json::Value>;

    // Drawing through Lua.
    /// The Lua state is up.
    fn has_lua(&self) -> bool;
    /// `bone.ui` functions that failed, skipped until the UI is refreshed.
    fn ui_broken(&mut self) -> &mut HashSet<String>;
    /// Bumped when views, or options and colors, change: part of every
    /// cached chat row's key.
    fn generation(&self) -> (u64, u64);
    /// When the chat on screen next needs drawing for a view's timer.
    fn chat_expiry(&mut self) -> &mut Option<Instant>;
    /// When the UI next needs a frame without redrawing cached chat rows
    /// (`bone.ui.refresh_in`).
    fn ui_expiry(&mut self) -> &mut Option<Instant>;
    /// The Lua state, for reading `bone.ui` without calling the API.
    fn lua(&self) -> Option<Lua>;
    /// The spinner's frames (`bone.ui.spinner`), read once per frame.
    fn spinner(&mut self) -> &mut crate::model::Spinner;
    /// How many `bone.job` jobs are running.
    fn jobs_running(&self) -> usize;
    /// The `bone.job` jobs.
    fn jobs(&mut self) -> &mut crate::jobs::Jobs;
    /// A view's `block` (a frontend's own drawing, such as the GUI's rich
    /// text) as rows `width` columns wide; `None` draws its `lines` instead.
    fn view_block(&mut self, _block: &serde_json::Value, _width: usize) -> Option<Vec<Self::Line>> {
        None
    }
    /// Where job tasks report: each event is to reach `jobs::event` on the
    /// UI thread.
    fn job_sink(&self) -> crate::jobs::Sink;
    /// The width of the whole window, in columns.
    fn screen_cols(&self) -> u16;
    /// Chat `buf`'s prompt has the keyboard (it is on screen and no popup
    /// or panel has it).
    fn prompt_focused(&self, buf: usize) -> bool;
    /// Where each layout leaf, and each column separator (with the path to
    /// the column after it), was drawn last frame, in character units.
    /// The GUI has no cell layout, so this is empty there.
    fn geometry(&self) -> Geometry {
        Geometry::default()
    }
    /// The open Lua windows.
    fn popups(&mut self) -> &mut Vec<crate::popup::Popup>;
    /// The chat item at cell `(x, y)` and the line of it there (both from
    /// 1), from where the chat on screen is scrolled. The GUI's node view
    /// has no cells under the pointer, so this is `None` there.
    fn chat_at(&self, _x: u16, _y: u16) -> Option<(usize, usize)> {
        None
    }
    /// The panels, and which one has the keyboard.
    fn panel_store(&mut self) -> panels::PanelStore<'_>;
    /// The known plugins and their shutdown hooks.
    fn plugin_store(&mut self) -> plugins::PluginStore<'_>;
    /// What a plugin, the user's config or a project runs in this
    /// frontend, in order: `ui.lua`, then its own (`tui.lua`, `gui.lua`).
    fn config_files(&self) -> &'static [&'static str];
    /// Which frontend this is: whose library it draws with.
    fn side(&self) -> bone_lua::Side;
    /// `/tui use` chose another library (`prev` was in use): load the UI
    /// again, outside the op, and go back to `prev` if it does not load.
    fn library_changed(&mut self, prev: String);
    /// Where this frontend runs, for finding a project's `.bone/`.
    fn cwd(&self) -> &str;
    /// Stop job `id` without calling back; the callbacks it held.
    fn cancel_job(&mut self, id: u64) -> Vec<u64>
    where
        Self: Sized,
    {
        jobs::cancel(self, id);
        self.jobs().forget_callbacks(id)
    }
    /// Keys typed toward a sequence go through now (the focus is moving).
    fn flush_keys(&mut self)
    where
        Self: Sized,
    {
        input::flush_pending(self);
    }

    // Keys (see `input`).
    /// Where the keyboard is and the key sequence in progress.
    fn key_state(&mut self) -> &mut input::KeyState;
    /// A key arrived, before anything handles it.
    fn key_starting(&mut self) {}
    /// A key or action is done: the prompt may have changed.
    fn prompt_changed(&mut self) {}
    /// Keys outside a window clear a shown message.
    fn dismiss_message(&mut self) {}
    /// A key no keymap took, in context `ctx`: text goes to the prompt
    /// while it has the keyboard.
    fn type_key(&mut self, ctx: &Context, key: Key);
    /// ctrl+c with nothing else to do.
    fn interrupt(&mut self);
    /// Before any part of built-in action `b` runs.
    fn builtin_starting(&mut self, _b: Builtin) {}
    /// What `b` does to a selection in the prompt; true if that was all.
    fn builtin_on_selection(&mut self, _b: Builtin) -> bool {
        false
    }
    /// What built-in action `b` does when neither a panel nor Lua took it.
    fn builtin_action(&mut self, b: Builtin);
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
        "request" => request(h, lua, a),
        "wait" => wait(h, lua, a),
        "notify" => notify(h, lua, a),
        "settings_get" => json(lua, h.settings()),
        "session" => session(h, lua),
        "prompt_get" => ret(lua, h.prompt_text()),
        "prompt_set" => args::<String>(lua, a).and_then(|text| {
            h.set_prompt_text(&text);
            ret(lua, ())
        }),
        "prompt_info" => h.prompt_info(lua).and_then(|t| ret(lua, t)),
        "prompt_empty" => ret(lua, h.prompt_empty()),
        "history_add" => args::<String>(lua, a).and_then(|text| {
            h.history_add(&text);
            ret(lua, ())
        }),
        "prompt_images" => prompt_images(h, lua, a),
        "panel_focused" => ret(lua, h.focused_panel()),
        "health" => json(lua, &serde_json::Value::Array(h.frontend_health())),
        "clipboard_copy" => args::<String>(lua, a).and_then(|text| {
            h.clipboard_copy(text);
            ret(lua, ())
        }),
        "ui_refresh_in" => args::<f64>(lua, a).and_then(|ms| {
            let at = Instant::now() + Duration::from_millis(ms.max(0.0) as u64);
            let e = h.ui_expiry();
            *e = Some(e.map_or(at, |e| e.min(at)));
            ret(lua, ())
        }),
        "ui_refresh" => {
            h.refresh_ui();
            ret(lua, ())
        }
        "process_list" => process_list(h, lua),
        "chat_view" => json(lua, &h.chat_view()),
        "geometry" => json(lua, &geometry(&h.geometry())),
        "popup_open" => popup_open(h, lua, a),
        "popup_update" => popup_update(h, lua, a),
        "popup_is_open" => popup_is_open(h, lua, a),
        "popup_close" => popup_close(h, lua, a),
        "on" => on(h, lua, a),
        "off" => off(h, lua, a),
        "command_create" => command_create(h, lua, a),
        "command_del" => command_del(h, lua, a),
        "command_list" => command_list(h, lua),
        "command_find" => command_find(h, lua, a),
        "command_complete" => command_complete(h, lua, a),
        "cmd" => cmd(h, lua, a),
        _ => {
            return panels::dispatch(h, lua, op, a)
                .or_else(|a| input::dispatch(h, lua, op, a))
                .or_else(|a| processes::dispatch(h, lua, op, a))
                .or_else(|a| jobs::dispatch(h, lua, op, a))
                .or_else(|a| libraries::dispatch(h, lua, op, a))
                .or_else(|a| plugins::dispatch(h, lua, op, a));
        }
    })
}

// ---- helpers ---------------------------------------------------------------

pub fn args<T: FromLuaMulti>(lua: &Lua, args: MultiValue) -> mlua::Result<T> {
    T::from_lua_multi(args, lua)
}

pub fn ret(lua: &Lua, v: impl IntoLuaMulti) -> mlua::Result<MultiValue> {
    v.into_lua_multi(lua)
}

/// Return `v` to Lua as a value.
pub fn json(lua: &Lua, v: &serde_json::Value) -> mlua::Result<MultiValue> {
    ret(lua, to_lua(lua, v)?)
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

/// Run the `submit` handlers on a message about to be sent. `None` when one
/// returned `false` (cancel); else the text to send, as a handler that
/// returned a string rewrote it.
pub fn fire_submit<H: Host>(h: &mut H, text: &str, images: &[ImageAttachment]) -> Option<String> {
    let data = serde_json::json!({ "text": text, "images": images });
    let mut text = text.to_owned();
    for v in fire(h, "submit", data) {
        match v {
            Value::Boolean(false) => return None,
            Value::String(s) => text = s.to_string_lossy(),
            _ => {}
        }
    }
    Some(text)
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
        default: spec.get::<Option<bool>>("default")?.unwrap_or(false),
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
        .or_else(|| bone_lua::frontend_runtime_source(dir.as_deref(), h.side(), &rel))
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

// ---- the core and the frontend ---------------------------------------------

/// `images = {}` from Lua is an empty object; requests want a list.
fn normalize_image_lists(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(images) = map.get_mut("images")
                && images.as_object().is_some_and(|o| o.is_empty())
            {
                *images = serde_json::json!([]);
            }
            for value in map.values_mut() {
                normalize_image_lists(value);
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                normalize_image_lists(value);
            }
        }
        _ => {}
    }
}

/// Call one-shot callback `cb` with a result, `(value)` or `(nil, error)`,
/// as its owner; then forget it.
fn deliver<H: Host>(h: &mut H, cb: u64, r: Result<serde_json::Value, String>, context: &str) {
    let called = h.as_owner_of(cb, |h| {
        h.with_api(|lua| {
            let cbs: Table = lua.named_registry_value(CALLBACKS)?;
            // Gone with a Lua state a reload replaced or rejected.
            let Some(f) = cbs.get::<Option<Function>>(cb)? else {
                return Ok(());
            };
            cbs.set(cb, Value::Nil)?;
            match r {
                Ok(v) => f.call::<()>(to_lua(lua, &v)?),
                Err(e) => f.call::<()>((Value::Nil, e)),
            }
        })
    });
    h.owners().callback_owner.remove(&cb);
    if let Err(e) = called {
        h.lua_error(context, &e);
    }
}

fn request<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let (method, params, f): (String, Value, Option<Function>) = args(lua, a)?;
    let mut params = from_lua(&params)?;
    normalize_image_lists(&mut params);
    let cb = f.map(|f| store_callback(h, lua, f)).transpose()?;
    let plugin = matches!(
        method.as_str(),
        "plugin/load" | "plugin/unload" | "plugin/reload"
    );
    // Plugin files changed on purpose, before and in the callback.
    if plugin {
        h.plugin_files_changing();
    }
    h.request_raw(method.clone(), params, move |h, r| {
        if plugin {
            h.plugin_files_changing();
        }
        match cb {
            Some(cb) => deliver(h, cb, r, &format!("{method} callback")),
            None => {
                if let Err(e) = r {
                    h.error(format!("{method}: {e}"));
                }
            }
        }
    });
    ret(lua, ())
}

/// Background work (bone.system, bone.http, bone.defer), then the callback
/// with the result, or (nil, error).
fn wait<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let (spec, f): (Value, Option<Function>) = args(lua, a)?;
    let spec = from_lua(&spec)?;
    let cb = f.map(|f| store_callback(h, lua, f)).transpose()?;
    h.spawn(bone_lua::wait::run(spec), move |h, r| match cb {
        Some(cb) => deliver(h, cb, r, "callback"),
        None => {
            if let Err(e) = r {
                h.error(e);
            }
        }
    });
    ret(lua, ())
}

fn notify<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let (msg, level): (String, Option<String>) = args(lua, a)?;
    if level.as_deref() == Some("error") {
        h.error(msg);
    } else {
        h.info(msg);
    }
    ret(lua, ())
}

fn session<H: Host>(h: &mut H, lua: &Lua) -> mlua::Result<MultiValue> {
    let target = h.target_chat();
    let c = h.chat_store();
    let Some(c) = target.and_then(|b| c.chats.get(b)) else {
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

fn prompt_images<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    if a.is_empty() {
        let images = serde_json::to_value(h.prompt_images()).unwrap_or_default();
        return ret(lua, to_lua(lua, &images)?);
    }
    let value: Value = args(lua, a)?;
    let images: Vec<ImageAttachment> = serde_json::from_value({
        let value = from_lua(&value)?;
        if value.as_object().is_some_and(|m| m.is_empty()) {
            serde_json::json!([])
        } else {
            value
        }
    })
    .map_err(|e| err(e.to_string()))?;
    h.set_prompt_images(images).map_err(err)?;
    ret(lua, ())
}

fn process_list<H: Host>(h: &mut H, lua: &Lua) -> mlua::Result<MultiValue> {
    let list: Vec<serde_json::Value> = h
        .process_list()
        .into_iter()
        .map(|p| serde_json::to_value(p).unwrap_or_default())
        .collect();
    ret(lua, to_lua(lua, &serde_json::Value::Array(list))?)
}

// ---- drawing chats through Lua views ---------------------------------------

/// Run Lua that may fail; a failure is reported once and `name` is skipped
/// until the UI is refreshed (`bone.ui.refresh`, or redefining it).
pub fn guarded<H: Host, R>(
    h: &mut H,
    name: &str,
    body: impl FnOnce(&Lua) -> mlua::Result<R>,
) -> Option<R> {
    if !h.has_lua() || h.ui_broken().contains(name) {
        return None;
    }
    match h.with_api(body) {
        Ok(r) => Some(r),
        Err(e) => {
            h.ui_broken().insert(name.to_owned());
            h.lua_error(
                &format!("bone.ui.{name} failed (plain text until it is redefined)"),
                &e,
            );
            None
        }
    }
}

/// What part of a chat is on screen: its first row, how many rows, and
/// whether it follows the end.
#[derive(Clone, Copy, Debug)]
pub struct ChatView {
    pub top: usize,
    pub height: usize,
    pub follow: bool,
}

/// How long one frame spends rendering items that are not on screen; the
/// rest wait for the next frame, so opening a long session stays quick.
const RENDER_BUDGET: Duration = Duration::from_millis(25);

/// Forget the cached items of chat `buf` whose view read chat data that has
/// changed since (`bone.chat.items` and `bone.chat.item` record what a view
/// read); returns the data generation they were checked at.
pub fn check_chat_deps<H: Host>(h: &mut H, buf: usize) -> u64 {
    // Views that read other chat data are drawn again when it changed.
    // Unchanged since the last walk: nothing cached can have gone stale.
    let data = crate::data::generation(h.chat_store().chats);
    if h.chat_store()
        .chats
        .get(buf)
        .is_some_and(|c| c.deps_at == Some(data))
    {
        return data;
    }
    let c = h.chat_store();
    let checked: Vec<_> = {
        // Each chat is indexed once, not once per view that read it.
        let mut chats = HashMap::new();
        c.chats[buf]
            .unchecked_deps(data)
            .into_iter()
            .map(|(item, deps)| {
                let fresh = deps.iter().all(|d| {
                    let session = &d.filter.session;
                    let chat = (chats.entry(session.clone()))
                        .or_insert_with(|| c.data_chat(session.as_deref()).map(Indexed::new));
                    chat.as_ref().map_or(0, |c| c.signature(&d.filter)) == d.sig
                });
                (item, fresh)
            })
            .collect()
    };
    for (item, fresh) in checked {
        if fresh {
            c.chats[buf].mark_checked(item, data);
        } else {
            c.chats[buf].invalidate(item);
        }
    }
    c.chats[buf].deps_at = Some(data);
    data
}

/// Render the stale items of chat `buf` through `bone.ui.views` at
/// `width_cols` and cache them: the ones on screen (`view`) first, then the
/// others nearest to them, for up to [`RENDER_BUDGET`]; if some are left
/// the chat asks for another frame. `line` lays out one line of a view's
/// items; `bare` is an item's rows when no view applies. Returns the top
/// row that keeps the same text at the top of the screen.
pub fn render_chat<H: Host>(
    h: &mut H,
    buf: usize,
    width_cols: usize,
    view: ChatView,
    line: &impl Fn(&H, &[crate::model::Item], usize) -> H::Line,
    bare: &impl Fn(&serde_json::Value, usize, bool) -> Vec<H::Line>,
) -> usize {
    let generation = h.generation();
    let data = check_chat_deps(h, buf);
    let c = h.chat_store();
    let stale = c.chats[buf].stale_streaming(width_cols, generation, Instant::now());
    let items = c.chats[buf].items();
    let anchor = (!view.follow)
        .then(|| c.chats[buf].item_at_row(view.top))
        .flatten();
    let (stale, visible) = visible_first(&c.chats[buf], &items, stale, view, anchor);
    let start = Instant::now();
    let mut pending = false;
    for (n, (item, index, key)) in stale.into_iter().enumerate() {
        if n >= visible && start.elapsed() >= RENDER_BUDGET {
            pending = true;
            break;
        }
        let c = h.chat_store();
        let prev = index
            .checked_sub(2)
            .and_then(|p| items.get(p))
            .map(|p| c.chats[buf].kind_name(p).to_owned());
        *c.render_deps = Some(Vec::new());
        *c.render_expires = None;
        let lines = render_item(h, buf, item, index, &key, prev, line, bare);
        let c = h.chat_store();
        let deps = c.render_deps.take().unwrap_or_default();
        let expires = c.render_expires.take();
        c.chats[buf].store(item, key, lines, deps, expires, data);
    }
    let expiry = {
        let chat = &h.chat_store().chats[buf];
        chat.next_expiry()
            .into_iter()
            .chain(chat.next_stream_expiry_after(Instant::now()))
            .min()
    };
    let now = pending.then(Instant::now);
    let at = h.chat_expiry();
    *at = (*at).into_iter().chain(expiry).chain(now).min();
    // Rows drawn above the top move it down by as many.
    let rows = h.chat_store().chats[buf].item_rows();
    anchor
        .and_then(|(index, line)| {
            let &(_, start, len) = rows.iter().find(|r| r.0 == index)?;
            Some(start + (line - 1).min(len.saturating_sub(1)))
        })
        .unwrap_or(view.top)
}

/// `stale` reordered so the items on screen come first, then the rest by
/// how far they are from the screen; and how many are on screen. Where an
/// item has no rows yet, it is taken to be one row.
fn visible_first<L>(
    chat: &ChatBuffer<L>,
    items: &[ChatItem],
    stale: Vec<(ChatItem, usize, RenderKey)>,
    view: ChatView,
    anchor: Option<(usize, usize)>,
) -> (Vec<(ChatItem, usize, RenderKey)>, usize) {
    if stale.is_empty() {
        return (stale, 0);
    }
    // Indexes are 1-based. Following, the screen ends at the last item;
    // otherwise it starts at the item at the top.
    let from = match anchor {
        Some((index, _)) => index,
        None if view.follow => items.len(),
        None => 1,
    };
    let mut on_screen = HashSet::new();
    let mut rows = 0;
    let mut i = from;
    while (1..=items.len()).contains(&i) && rows < view.height.max(1) {
        on_screen.insert(i);
        rows += chat.cached_rows(&items[i - 1]).unwrap_or(1);
        i = if view.follow && anchor.is_none() {
            i - 1
        } else {
            i + 1
        };
    }
    let mut stale = stale;
    stale.sort_by_key(|&(_, index, _)| (!on_screen.contains(&index), index.abs_diff(from)));
    let visible = stale.iter().filter(|s| on_screen.contains(&s.1)).count();
    (stale, visible)
}

#[allow(clippy::too_many_arguments)]
fn render_item<H: Host>(
    h: &mut H,
    buf: usize,
    item: ChatItem,
    index: usize,
    key: &RenderKey,
    prev: Option<String>,
    line: &impl Fn(&H, &[crate::model::Item], usize) -> H::Line,
    bare: &impl Fn(&serde_json::Value, usize, bool) -> Vec<H::Line>,
) -> Vec<H::Line> {
    let c = h.chat_store();
    let data = c.chats[buf].item_data(item, index, true);
    let kind = c.chats[buf].kind_name(&item).to_owned();
    let ctx = serde_json::json!({
        "width": key.width,
        "region": "chat",
        "prev": prev.map(|kind| serde_json::json!({ "kind": kind })),
    });
    let lines = guarded(h, &format!("views.{kind}"), |lua| {
        let views: Table = lua
            .globals()
            .get::<Table>("bone")?
            .get::<Table>("ui")?
            .get("views")?;
        let Some(f) = views.get::<Option<Function>>(kind.as_str())? else {
            return Ok(None);
        };
        let v = f.call::<Value>((to_lua(lua, &data)?, to_lua(lua, &ctx)?))?;
        // A frontend's own block (`{ block = …, lines = … }`): the lines are
        // for a frontend that does not draw it.
        if let Value::Table(t) = &v
            && let Some(block) = t.get::<Option<Value>>("block")?
        {
            let block = bone_lua::from_lua(&block)?;
            let lines = crate::model::parse_lines(t.get("lines")?)?.unwrap_or_default();
            return Ok(Some((Some(block), lines)));
        }
        // nil hides the item.
        Ok(Some((
            None,
            crate::model::parse_lines(v)?.unwrap_or_default(),
        )))
    })
    .flatten();
    match lines {
        Some((Some(block), lines)) => match h.view_block(&block, key.width) {
            Some(rows) => rows,
            None => lines.iter().map(|l| line(h, l, key.width)).collect(),
        },
        Some((None, lines)) => lines.iter().map(|l| line(h, l, key.width)).collect(),
        None => bare(&data, key.width, key.prev.is_none()),
    }
}

// ---- running commands ------------------------------------------------------

/// Send typed request `M`; `then` gets its typed result on the UI thread.
pub(crate) fn request_typed<H: Host, M: bone_proto::Method>(
    h: &mut H,
    params: M::Params,
    then: impl FnOnce(&mut H, Result<M::Result, String>) + Send + 'static,
) {
    let params = match serde_json::to_value(params) {
        Ok(p) => p,
        Err(e) => return then(h, Err(e.to_string())),
    };
    h.request_raw(M::METHOD.to_owned(), params, move |h, r| {
        let r = r.and_then(|v| serde_json::from_value(v).map_err(|e| e.to_string()));
        then(h, r)
    });
}

fn cmd<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let line: String = args(lua, a)?;
    execute(h, &line);
    ret(lua, ())
}

/// Run a command line (with or without the leading `/`): `/model`, or a
/// command Lua defined, with its arguments parsed by its spec.
pub fn execute<H: Host>(h: &mut H, line: &str) {
    let line = line.trim().trim_start_matches('/');
    let (word, args) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    let args = args.trim();
    if word == "model" {
        return model_command(h, args);
    }
    let Some((name, cmd)) = h
        .user_commands()
        .iter()
        .find(|(canonical, command)| {
            canonical.as_str() == word || command.aliases.iter().any(|alias| alias == word)
        })
        .map(|(name, command)| (name.clone(), command.clone()))
    else {
        return h.error(format!("Unknown command /{word}"));
    };
    let raw_args = args.to_owned();
    let parsed = match crate::commands::structured_args(cmd.args.as_ref(), args) {
        Ok(parsed) => parsed,
        Err(e) => return h.error(e),
    };
    let argv = serde_json::Value::Array(
        args.split_whitespace()
            .map(|arg| serde_json::Value::String(arg.to_owned()))
            .collect(),
    );
    h.call_callback(cmd.callback, &format!("/{name}"), |lua| {
        let t = lua.create_table()?;
        t.set("args", raw_args)?;
        t.set("argv", to_lua(lua, &argv)?)?;
        t.set("command", name.as_str())?;
        if let Some(parsed) = parsed {
            let value = to_lua(lua, &parsed)?;
            t.set("arguments", value.clone())?;
            // `parsed` is an explicit alias for plugins that prefer not to
            // overload the word "arguments".
            t.set("parsed", value)?;
        }
        Ok(Value::Table(t))
    });
}

/// `/model`: show the current provider's model, or set it.
fn model_command<H: Host>(h: &mut H, args: &str) {
    use bone_proto::methods::{MaybeSession, ModelList, SettingSet, SettingsSet};
    let c = h.chat_store();
    let session_id = c.chats[c.current].session_id().map(str::to_owned);
    let args = args.to_owned();
    request_typed::<H, ModelList>(
        h,
        MaybeSession {
            session_id: session_id.clone(),
        },
        move |h, r| {
            let models = match r {
                Ok(models) => models,
                Err(e) => return h.error(format!("cannot list models: {e}")),
            };
            let Some(current) = models.into_iter().find(|model| model.current) else {
                return h.error("no model provider is configured".into());
            };
            if args.is_empty() {
                return h.info(format!("{} ({})", current.model, current.name));
            }
            let model = args.clone();
            request_typed::<H, SettingsSet>(
                h,
                SettingSet {
                    path: format!("providers.{}.model", current.name),
                    value: serde_json::Value::String(args),
                    session_id,
                },
                move |h, r| match r {
                    Ok(_) => h.info(format!("model: {} ({})", model, current.name)),
                    Err(e) => h.error(format!("cannot set model: {e}")),
                },
            );
        },
    );
}

/// `bone.ui.geometry()`: the leaves drawn last frame (empty ones left out)
/// and the column separators.
fn geometry(g: &Geometry) -> serde_json::Value {
    let rect = |r: &crate::layout::Rect| serde_json::json!({ "x": r.x, "y": r.y, "width": r.width, "height": r.height });
    let leaves: Vec<_> = (g.leaves.iter())
        .filter(|(_, r)| r.width > 0 && r.height > 0)
        .map(|(name, r)| {
            let mut v = rect(r);
            v["name"] = name.clone().into();
            v
        })
        .collect();
    let seps: Vec<_> = (g.seps.iter())
        .map(|(r, path)| {
            let mut v = rect(r);
            v["path"] = path.clone().into();
            v
        })
        .collect();
    serde_json::json!({ "leaves": leaves, "seps": seps })
}

// ---- windows ---------------------------------------------------------------

use crate::popup::{self, Popup};

/// The focus moved: `focus/changed` with what has the keyboard.
pub fn emit_focus_changed<H: Host>(h: &mut H) {
    let data = serde_json::json!({
        "context": h.context().name(),
        "popup": popup::focused(h.popups()).is_some(),
        "panel": h.focused_panel(),
    });
    fire(h, "focus/changed", data);
}

fn focused_popup<H: Host>(h: &mut H) -> Option<u64> {
    popup::focused(h.popups()).map(|p| p.id)
}

/// Set the fields of a window that `spec` names (for opening and
/// `bone.ui.update`). Replaced callbacks are released.
fn apply_win_spec<H: Host>(h: &mut H, lua: &Lua, p: &mut Popup, spec: &Table) -> mlua::Result<()> {
    match spec.get::<Value>("lines")? {
        Value::Nil => {}
        v @ (Value::Function(_) | Value::Table(_)) => {
            if p.lines != 0 {
                drop_callback(lua, p.lines)?;
            }
            let next = h.owners().next_callback;
            *next += 1;
            p.lines = *next;
            lua.named_registry_value::<Table>(CALLBACKS)?
                .set(p.lines, v)?;
        }
        _ => return Err(err("window lines must be a list or a function")),
    }
    if let Some(t) = spec.get::<Option<Table>>("keys")? {
        for (_, cb) in p.keys.drain(..) {
            drop_callback(lua, cb)?;
        }
        for pair in t.pairs::<String, Function>() {
            let (name, f) = pair?;
            let key = keys::parse(&name).map_err(err)?;
            p.keys.push((key, store_callback(h, lua, f)?));
        }
    }
    if let Some(f) = spec.get::<Option<Function>>("on_key")? {
        if let Some(cb) = p.on_key.take() {
            drop_callback(lua, cb)?;
        }
        p.on_key = Some(store_callback(h, lua, f)?);
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
        p.guard = Duration::from_millis(ms);
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
    h.changed();
    Ok(())
}

fn popup_open<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let spec: Table = args(lua, a)?;
    let old_focus = focused_popup(h);
    let next = h.owners().next_callback;
    *next += 1;
    let mut p = Popup::new(*next);
    if !matches!(
        spec.get::<Value>("lines")?,
        Value::Function(_) | Value::Table(_)
    ) {
        return Err(err("a window needs lines: a list or a function"));
    }
    apply_win_spec(h, lua, &mut p, &spec)?;
    let id = p.id;
    let event = p.event();
    own(h, Owned::Popup(id));
    h.popups().push(p);
    fire(h, "panel/opened", event);
    if old_focus != focused_popup(h) {
        emit_focus_changed(h);
    }
    h.changed();
    ret(lua, id)
}

fn popup_update<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let (id, spec): (u64, Table) = args(lua, a)?;
    let Some(i) = h.popups().iter().position(|p| p.id == id) else {
        return ret(lua, false);
    };
    let old_focus = focused_popup(h);
    let mut p = h.popups().remove(i);
    p.rect = None;
    if let Err(error) = apply_win_spec(h, lua, &mut p, &spec) {
        h.popups().insert(i, p);
        return Err(error);
    }
    let event = p.event();
    h.popups().insert(i, p);
    fire(h, "panel/updated", event);
    if old_focus != focused_popup(h) {
        emit_focus_changed(h);
    }
    h.changed();
    ret(lua, true)
}

fn popup_is_open<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let id: u64 = args(lua, a)?;
    ret(lua, h.popups().iter().any(|p| p.id == id))
}

fn popup_close<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let id: u64 = args(lua, a)?;
    if let Some(i) = h.popups().iter().position(|p| p.id == id) {
        let old_focus = focused_popup(h);
        let p = h.popups().remove(i);
        let event = p.event();
        drop_callback(lua, p.lines)?;
        if let Some(cb) = p.on_key {
            drop_callback(lua, cb)?;
        }
        for (_, cb) in p.keys {
            drop_callback(lua, cb)?;
        }
        fire(h, "panel/closed", event);
        if old_focus != focused_popup(h) {
            emit_focus_changed(h);
        }
        h.changed();
    }
    ret(lua, ())
}

/// Size and place every window, bottom first, recording where each went;
/// what to paint, in that order: its id, area and lines.
pub fn layout_popups<H: Host>(
    h: &mut H,
    anchors: &popup::Anchors,
) -> Vec<(u64, crate::layout::Rect, Vec<Vec<crate::model::Item>>)> {
    // Skipped or zero-sized windows must not keep bounds from an earlier frame.
    for p in h.popups().iter_mut() {
        p.rect = None;
    }
    let order: Vec<Popup> = popup::in_order(h.popups()).into_iter().cloned().collect();
    let mut out = Vec::new();
    for p in order {
        let placed = popup::place(&p, anchors, |w, height| {
            let lines = ui::popup_lines(h, p.id, p.lines, w, height);
            let width = (lines.iter())
                .map(|l| {
                    crate::model::layout_items(l, w as usize)
                        .iter()
                        .map(|(t, _)| crate::text::width(t))
                        .sum::<usize>()
                })
                .max()
                .unwrap_or(0) as u16;
            (lines, width)
        });
        let Some((rect, lines)) = placed else {
            continue;
        };
        // A render callback may have closed the window.
        let Some(q) = h.popups().iter_mut().find(|q| q.id == p.id) else {
            continue;
        };
        q.rect = Some(rect);
        out.push((p.id, rect, lines));
    }
    out
}

//! The TUI's Lua state and the `bone._api` bridge.
//!
//! Lua code only ever runs inside [`App::with_api`], which lends the whole
//! `App` to a scoped dispatch function for the duration of the call. The
//! persistent `bone._api(op, ...)` forwards to whichever dispatcher is
//! current, so API calls act on live state and nested calls (a mapping that
//! runs a command that calls Lua) reborrow safely.

use std::path::PathBuf;

use bone_lua::{Side, short_error, to_lua};
use mlua::{Function, Lua, MultiValue, Table, Value};

use crate::app::App;
use crate::keymap::{Builtin, Context};
use bone_ui::lua::{CALLBACKS, args, err, ret};
use serde_json::Value as Json;

/// What a plugin, the user's config, or a project runs in the TUI, in
/// order: the UI code it shares with the GUI, then the TUI's own.
pub const CONFIG_FILES: [&str; 2] = ["ui.lua", "tui.lua"];

use bone_ui::lua::canonical_event;
pub use bone_ui::lua::{Autocmd, UserCommand};

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

    /// Run `f` as the plugin that owns callback `cb` (or as the user's
    /// config), so what it creates belongs to that plugin.
    pub fn as_owner_of<R>(&mut self, cb: u64, f: impl FnOnce(&mut App) -> R) -> R {
        <App as bone_ui::lua::Host>::as_owner_of(self, cb, f)
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

fn dispatch(app: &mut App, lua: &Lua, op: &str, a: MultiValue) -> mlua::Result<MultiValue> {
    let a = match bone_ui::lua::dispatch(app, lua, op, a) {
        Ok(r) => return r,
        Err(a) => a,
    };
    match op {
        "clipboard_paste" => {
            app.paste_clipboard();
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

    fn processes(&mut self) -> &mut bone_ui::lua::processes::Processes {
        &mut self.processes
    }

    fn chat_view(&self) -> serde_json::Value {
        App::chat_view(self)
    }

    fn set_prompt_text(&mut self, text: &str) {
        App::set_prompt_text(self, text);
    }

    fn prompt_info(&mut self, lua: &Lua) -> mlua::Result<Table> {
        let t = lua.create_table()?;
        t.set("text", self.prompt.text())?;
        t.set(
            "images",
            to_lua(lua, &serde_json::to_value(self.prompt_images()).unwrap())?,
        )?;
        t.set("lines", self.prompt.lines().to_vec())?;
        t.set("cursor", pos_out(lua, self.prompt.cursor())?)?;
        t.set("history", self.prompt_history_active())?;
        if let Some((a, b)) = self.prompt.selection() {
            let sel = lua.create_table()?;
            sel.set("start", pos_out(lua, a)?)?;
            sel.set("end", pos_out(lua, b)?)?;
            sel.set("text", self.prompt.range_text(a, b))?;
            t.set("selection", sel)?;
        }
        Ok(t)
    }

    fn prompt_empty(&self) -> bool {
        self.draft_empty()
    }

    fn history_add(&mut self, text: &str) {
        App::history_add(self, text);
    }

    /// Copied after the next draw, like a mouse selection.
    fn clipboard_copy(&mut self, text: String) {
        (self.clipboard, self.dirty) = (Some(text), true);
    }

    fn frontend_health(&self) -> Vec<serde_json::Value> {
        self.tui_health()
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

    fn ui_expiry(&mut self) -> &mut Option<std::time::Instant> {
        &mut self.ui_expiry
    }

    fn lua(&self) -> Option<Lua> {
        self.lua.clone()
    }

    fn spinner(&mut self) -> &mut crate::ui::Spinner {
        &mut self.spinner
    }

    fn jobs_running(&self) -> usize {
        self.jobs.running()
    }

    fn screen_cols(&self) -> u16 {
        self.screen.width
    }

    fn prompt_focused(&self, buf: usize) -> bool {
        buf == self.current && self.focused_popup().is_none() && self.focused_panel().is_none()
    }

    fn popups(&mut self) -> &mut Vec<bone_ui::popup::Popup> {
        &mut self.popups
    }

    fn chat_at(&self, x: u16, y: u16) -> Option<(usize, usize)> {
        let hit = App::chat_at(self, (x, y))?;
        Some((
            hit["index"].as_u64()? as usize,
            hit["line"].as_u64()? as usize,
        ))
    }

    fn plugin_store(&mut self) -> bone_ui::lua::plugins::PluginStore<'_> {
        bone_ui::lua::plugins::PluginStore {
            plugins: &mut self.plugins,
            shutdown_hooks: &mut self.shutdown_hooks,
        }
    }

    fn config_files(&self) -> &'static [&'static str] {
        &CONFIG_FILES
    }

    fn side(&self) -> bone_lua::Side {
        bone_lua::Side::Tui
    }

    fn library_changed(&mut self, prev: String) {
        self.library_prev = Some(prev);
        // Not from inside the op: the reload replaces its state.
        self.reload_requested = true;
    }

    fn cwd(&self) -> &str {
        &self.cwd
    }

    fn jobs(&mut self) -> &mut bone_ui::jobs::Jobs {
        &mut self.jobs
    }

    fn job_sink(&self) -> bone_ui::jobs::Sink {
        let tx = self.event_sender();
        std::sync::Arc::new(move |id, ev| {
            let _ = tx.send(crate::app::AppEvent(Box::new(move |app: &mut App| {
                bone_ui::lua::jobs::event(app, id, ev)
            })));
        })
    }

    fn panel_store(&mut self) -> bone_ui::lua::panels::PanelStore<'_> {
        bone_ui::lua::panels::PanelStore {
            panels: &mut self.panels,
            focus: &mut self.panel_focus,
        }
    }

    fn key_state(&mut self) -> &mut bone_ui::lua::input::KeyState {
        &mut self.input
    }

    fn key_starting(&mut self) {
        self.dirty = true;
        self.selection = None;
    }

    fn prompt_changed(&mut self) {
        self.emit_prompt_changed();
    }

    fn dismiss_message(&mut self) {
        if self.message.is_some() {
            self.message = None;
        }
    }

    fn type_key(&mut self, ctx: &Context, key: crate::keys::Key) {
        App::type_key(self, ctx, key);
    }

    fn interrupt(&mut self) {
        App::interrupt(self);
    }

    fn builtin_starting(&mut self, b: Builtin) {
        if b != Builtin::Interrupt {
            self.quit_armed = None;
        }
    }

    fn builtin_on_selection(&mut self, b: Builtin) -> bool {
        App::builtin_on_selection(self, b)
    }

    fn builtin_action(&mut self, b: Builtin) {
        App::builtin_action(self, b);
    }

    fn geometry(&self) -> bone_ui::lua::Geometry {
        let cells =
            |r: &ratatui::layout::Rect| bone_ui::layout::Rect::new(r.x, r.y, r.width, r.height);
        bone_ui::lua::Geometry {
            leaves: (self.leaves.iter())
                .map(|(n, r)| (n.clone(), cells(r)))
                .collect(),
            seps: (self.seps.iter())
                .map(|(r, path)| (cells(r), path.clone()))
                .collect(),
        }
    }
}

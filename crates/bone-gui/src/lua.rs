//! The GUI's Lua host. The same Lua API as the TUI's: `runtime/ui/api.lua`
//! over `bone._api`, whose shared ops are `bone_ui::lua`'s, run against the
//! GUI through [`Host`]. Ops only a frontend can do are here; one the GUI
//! does not have yet raises an error saying so.
//!
//! Load order, as the TUI's: the API, the defaults (`gui/defaults.lua`,
//! which sets up the standard UI, or the bare library's file), each
//! enabled plugin's `ui.lua` and `gui.lua`, then the user's `ui.lua` and
//! `gui.lua` (not with the bare library), then `ready`.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Instant;

use bone_lua::Side;
use bone_proto::methods::ProcessSnapshot;
use bone_proto::types::ImageAttachment;
use bone_ui::data::Dep;
use bone_ui::keymap::{Context, Keymaps};
use bone_ui::keys::Key;
use bone_ui::lua::{
    Autocmd, CALLBACKS, ChatStore, Host, KeymapStore, Looks, OptionStore, Owned, Owners,
    UserCommand, err,
};
use bone_ui::options::{DynamicOption, DynamicValue, Options};
use bone_ui::theme::Theme;
use mlua::{Function, Lua, MultiValue, Table, Value};
use serde_json::Value as Json;

use crate::{Gui, Msg, Row, wake};

/// What a plugin, the user's config, or a project runs in the GUI, in
/// order: the UI code it shares with the TUI, then the GUI's own.
pub const CONFIG_FILES: [&str; 2] = ["ui.lua", "gui.lua"];

/// The GUI's Lua state and what the Lua API keeps in it.
#[derive(Default)]
pub struct LuaSide {
    pub state: Option<Lua>,
    pub owner: Option<String>,
    pub callback_owner: HashMap<u64, String>,
    pub owned: HashMap<String, Vec<Owned>>,
    pub next_callback: u64,
    pub options: Options,
    pub dynamic_options: HashMap<String, DynamicOption>,
    pub carried_options: HashMap<String, DynamicValue>,
    pub opts_rev: u64,
    pub autocmds: Vec<Autocmd>,
    pub user_commands: HashMap<String, UserCommand>,
    pub keymaps: Keymaps,
    pub keymap_owner: HashMap<(Context, Vec<Key>), String>,
    pub theme: Theme,
    pub colors_name: Option<String>,
    pub views_rev: u64,
    pub ui_broken: HashSet<String>,
    pub rendering: Option<usize>,
    pub render_deps: Option<Vec<Dep>>,
    pub render_expires: Option<Instant>,
    pub chat_expiry: Option<Instant>,
    /// When `bone.ui.refresh_in` asked for the next frame.
    pub ui_expiry: Option<Instant>,
    pub lua_item_seq: u64,
    pub spinner: bone_ui::model::Spinner,
    /// The window's width in columns, as last drawn, and its size in
    /// columns and rows as `resize` last reported it.
    pub screen_cols: u16,
    pub screen_size: (u16, u16),
    /// Where the layout put things last frame.
    pub geometry: bone_ui::lua::Geometry,
    /// Lua windows (`bone.ui.popup`).
    pub popups: Vec<bone_ui::popup::Popup>,
    /// Where the keyboard is and the key sequence in progress.
    pub input: bone_ui::lua::input::KeyState,
    /// Where character cells were last frame: origin, column width, row
    /// height (logical pixels).
    pub cells: Option<(f32, f32, f32, f32)>,
    /// The first chat row in view, and a pointer button held for dragging.
    pub chat_top: usize,
    /// Each chat's first row in view and whether it is scrolled to the end,
    /// last frame.
    pub chat_views: HashMap<usize, (usize, bool)>,
    pub dragging: bool,
    /// The font file and size applied last (`bone.o.font`, `font_size`).
    pub fonts: Option<(String, f32)>,
    /// The menu bar Lua set (`bone.gui.menu`).
    pub menus: Vec<(String, Vec<crate::window::MenuItem>)>,
    /// Text copied before there was a window to copy it with (headless).
    pub copied: Option<String>,
    /// The prompt's text as `prompt/changed` last reported it.
    pub last_prompt: (
        String,
        Vec<ImageAttachment>,
        serde_json::Value,
        serde_json::Value,
    ),
    /// The known plugins and their shutdown hooks.
    pub plugins: Vec<bone_ui::lua::plugins::Plugin>,
    pub shutdown_hooks: Vec<(Option<String>, u64)>,
    /// Lua panels (`bone.ui.panel`).
    pub panels: Vec<bone_ui::panel::Panel>,
    pub panel_focus: Option<String>,
    /// Every message shown, and the last Lua errors in full.
    /// Streaming jobs from Lua (`bone.job`).
    pub jobs: bone_ui::jobs::Jobs,
    pub log: Vec<String>,
    pub lua_errors: Vec<String>,
    /// While the configuration loads: where each error happened and what
    /// it said, numbers blanked (see `failure_key`).
    load_errors: Option<Vec<String>>,
    /// Where the load in use had errors. A reload that fails only there is
    /// still taken, so an old error cannot pin the old configuration.
    load_failures: HashSet<String>,
}

/// What identifies a load error across reloads: where it happened and what
/// it said, with numbers blanked so the same error on another line (after
/// an edit above it) still matches.
fn failure_key(context: &str, message: &str) -> String {
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

impl Gui {
    /// Create the Lua state and load the configuration into it, noting
    /// where it had errors.
    pub(crate) fn load_lua(&mut self) {
        self.lua.load_errors = Some(Vec::new());
        self.load_lua_files();
        self.lua.load_failures = self
            .lua
            .load_errors
            .take()
            .unwrap_or_default()
            .into_iter()
            .collect();
    }

    /// Load the configuration again (after its files changed) into a fresh
    /// state while the current one waits. The new one is taken only if it
    /// loaded without an error the current one did not also have; otherwise
    /// the current one stays and the error is shown. Option values carry
    /// over; the chats, prompt and session are Rust's and stay.
    pub fn reload_lua(&mut self) -> bool {
        let carried = (self.lua.dynamic_options.iter())
            .map(|(name, o)| (name.clone(), o.value.clone()))
            .collect();
        let old = std::mem::take(&mut self.lua);
        // Later than anything cached under the old state.
        self.lua.views_rev = old.views_rev + 1;
        self.lua.opts_rev = old.opts_rev + 1;
        self.lua.carried_options = carried;
        self.lua.log = old.log.clone();
        self.lua.load_errors = Some(Vec::new());
        self.load_lua_files();
        let errors = self.lua.load_errors.take().unwrap_or_default();
        if let Some(new) = errors.iter().find(|e| !old.load_failures.contains(*e)) {
            let new = new.lines().next().unwrap_or_default().to_owned();
            let mut rejected = std::mem::replace(&mut self.lua, old);
            rejected.jobs.cancel_all();
            self.lua.log = rejected.log;
            self.lua.views_rev += 1;
            Gui::error(
                self,
                format!("Lua configuration not reloaded: {new} failed"),
            );
            return false;
        }
        self.lua.load_failures = errors.into_iter().collect();
        // The old state's jobs go with it.
        let mut old = old;
        old.jobs.cancel_all();
        Gui::info(self, "Lua configuration reloaded".into());
        true
    }

    fn load_lua_files(&mut self) {
        let dir = self.opts.config_dir.clone();
        match bone_ui::lua::state::new_state(Side::Gui, dir.as_deref()) {
            Ok(lua) => self.lua.state = Some(lua),
            Err(e) => return self.lua_error("lua", &e),
        }
        // Bare loads its own small file in place of the standard UI's.
        let library = dir.as_deref().and_then(|d| bone_lua::library(d, Side::Gui));
        let bare = library.as_deref() == Some("bare");
        let defaults = if bare {
            "gui/bare.lua"
        } else {
            "gui/defaults.lua"
        };
        for rel in ["ui/api.lua", "gui/api.lua", defaults] {
            let r = self.with_api(|lua| {
                let (src, name) = bone_lua::frontend_runtime_source(dir.as_deref(), Side::Gui, rel)
                    .ok_or_else(|| err(format!("missing runtime file {rel}")))?;
                lua.load(src).set_name(format!("@{name}")).exec()
            });
            if let Err(e) = r {
                self.lua_error(&format!("runtime/{rel}"), &e);
            }
        }
        if let Some(dir) = &dir {
            self.load_user_files(dir, bare);
        }
        bone_ui::lua::fire(self, "ready", Json::Null);
    }

    fn load_user_files(&mut self, dir: &Path, bare: bool) {
        // Plugins switched off in settings.json are not loaded.
        let disabled: Vec<String> = self.settings["plugins"]["disabled"]
            .as_array()
            .map(|l| {
                l.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        use bone_ui::lua::plugins::{self, Kind};
        for plugin in bone_lua::plugins(dir) {
            let name = bone_lua::plugin_name(&plugin);
            if disabled.contains(&name) {
                plugins::register_plugin(self, &name, &plugin, Kind::Plugin);
            } else {
                plugins::load_plugin(self, &name, &plugin, Kind::Plugin);
            }
        }
        // Bare is the plain UI: the user's files would build a full one.
        if !bare {
            self.run_files(dir);
            plugins::load_project(self);
        }
    }

    /// `ui.lua` then `gui.lua` in `dir`, stopping at an error.
    fn run_files(&mut self, dir: &Path) {
        for path in CONFIG_FILES.map(|f| dir.join(f)) {
            if let Err(e) = self.with_api(|lua| bone_lua::run_file(lua, &path)) {
                self.lua_error(&path.display().to_string(), &e);
                break;
            }
        }
    }

    /// Run Lua with the API bound to the GUI.
    pub fn with_api<R>(&mut self, body: impl FnOnce(&Lua) -> mlua::Result<R>) -> mlua::Result<R> {
        let Some(lua) = self.lua.state.clone() else {
            return Err(mlua::Error::runtime("Lua is not available"));
        };
        bone_ui::lua::state::lend(self, &lua, dispatch, body)
    }

    /// A Lua expression's value, as a string.
    pub fn eval(&mut self, expr: &str) -> Result<String, String> {
        self.with_api(|lua| {
            lua.load(format!("return tostring({expr})"))
                .eval::<String>()
        })
        .map_err(|e| e.to_string())
    }

    /// Run a Lua chunk named `name`; its error, if any.
    pub fn run_lua(&mut self, name: &str, code: &str) -> Result<(), String> {
        self.with_api(|lua| lua.load(code).set_name(name).exec())
            .map_err(|e| e.to_string())
    }

    /// Fire `event` to its Lua handlers, if it has any.
    pub(crate) fn fire_event(&mut self, event: &str, data: Json) {
        if self.lua.state.is_some() && bone_ui::lua::has_autocmd(self, event) {
            bone_ui::lua::fire(self, event, data);
        }
    }

    fn call_stored(
        &mut self,
        id: u64,
        context: &str,
        args: impl FnOnce(&Lua) -> mlua::Result<MultiValue>,
    ) -> Option<MultiValue> {
        let r = self.as_owner_of(id, |gui| {
            gui.with_api(|lua| {
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

    fn lua_error(&mut self, context: &str, e: &mlua::Error) {
        if let Some(errors) = &mut self.lua.load_errors {
            errors.push(failure_key(context, &bone_lua::short_error(e)));
        }
        self.lua.lua_errors.push(format!("{context}: {e}"));
        if self.lua.lua_errors.len() > 3 {
            self.lua.lua_errors.remove(0);
        }
        self.error(format!("{context}: {}", bone_lua::short_error(e)));
    }
}

/// `bone._api` in the GUI: the shared ops, then the GUI's own.
fn dispatch(gui: &mut Gui, lua: &Lua, op: &str, a: MultiValue) -> mlua::Result<MultiValue> {
    match bone_ui::lua::dispatch(gui, lua, op, a) {
        Ok(r) => r,
        Err(a) => (crate::images::dispatch(gui, lua, op, a.clone()))
            .or_else(|| crate::chats::dispatch(gui, lua, op, a.clone()))
            .or_else(|| crate::prompt::dispatch(gui, lua, op, a.clone()))
            .or_else(|| crate::window::dispatch(gui, lua, op, a))
            .unwrap_or_else(|| Err(err(format!("{op}: not available in the GUI yet")))),
    }
}

impl Host for Gui {
    type Line = Row;

    fn chat_store(&mut self) -> ChatStore<'_, Row> {
        ChatStore {
            chats: &mut self.chats,
            current: self.current,
            rendering: self.lua.rendering,
            render_deps: &mut self.lua.render_deps,
            render_expires: &mut self.lua.render_expires,
            lua_item_seq: &mut self.lua.lua_item_seq,
        }
    }

    fn owners(&mut self) -> Owners<'_> {
        let l = &mut self.lua;
        Owners {
            owner: &mut l.owner,
            callback_owner: &mut l.callback_owner,
            owned: &mut l.owned,
            next_callback: &mut l.next_callback,
        }
    }

    fn option_store(&mut self) -> OptionStore<'_> {
        let l = &mut self.lua;
        OptionStore {
            options: &mut l.options,
            dynamic: &mut l.dynamic_options,
            carried: &l.carried_options,
            rev: &mut l.opts_rev,
        }
    }

    fn autocmds(&mut self) -> &mut Vec<Autocmd> {
        &mut self.lua.autocmds
    }

    fn keymap_store(&mut self) -> KeymapStore<'_> {
        let l = &mut self.lua;
        KeymapStore {
            keymaps: &mut l.keymaps,
            owner: &mut l.keymap_owner,
        }
    }

    /// What has the keyboard: a focused window (`popup`), a focused panel
    /// (its context, or `panel`), else the prompt.
    fn context(&self) -> Context {
        let l = &self.lua;
        bone_ui::lua::input::context_of(&l.popups, &l.panels, &l.panel_focus, &l.input)
    }

    fn looks(&mut self) -> Looks<'_> {
        let l = &mut self.lua;
        Looks {
            theme: &mut l.theme,
            colors_name: &mut l.colors_name,
            rev: &mut l.opts_rev,
        }
    }

    fn config_dir(&self) -> Option<&Path> {
        self.opts.config_dir.as_deref()
    }

    fn with_api<R>(&mut self, body: impl FnOnce(&Lua) -> mlua::Result<R>) -> mlua::Result<R> {
        Gui::with_api(self, body)
    }

    fn user_commands(&mut self) -> &mut HashMap<String, UserCommand> {
        &mut self.lua.user_commands
    }

    fn call_callback(
        &mut self,
        id: u64,
        context: &str,
        arg: impl FnOnce(&Lua) -> mlua::Result<Value>,
    ) -> Option<Value> {
        self.call_stored(id, context, |lua| Ok(MultiValue::from_vec(vec![arg(lua)?])))
            .map(|v| v.into_iter().next().unwrap_or(Value::Nil))
    }

    fn call_callback_multi(
        &mut self,
        id: u64,
        context: &str,
        args: impl FnOnce(&Lua) -> mlua::Result<MultiValue>,
    ) -> Option<MultiValue> {
        self.call_stored(id, context, args)
    }

    fn changed(&mut self) {
        wake(&self.repaint);
    }

    fn request_raw(
        &mut self,
        method: String,
        params: Json,
        then: impl FnOnce(&mut Self, Result<Json, String>) + Send + 'static,
    ) {
        let (client, tx, repaint) = (self.client.clone(), self.tx.clone(), self.repaint.clone());
        self.rt.spawn(async move {
            let r = client
                .request_raw(&method, params)
                .await
                .map_err(|e| e.to_string());
            let _ = tx.send(Msg::Apply(Box::new(move |gui| then(gui, r))));
            wake(&repaint);
        });
    }

    fn spawn<T: Send + 'static>(
        &mut self,
        fut: impl std::future::Future<Output = T> + Send + 'static,
        then: impl FnOnce(&mut Self, T) + Send + 'static,
    ) {
        let (tx, repaint) = (self.tx.clone(), self.repaint.clone());
        self.rt.spawn(async move {
            let r = fut.await;
            let _ = tx.send(Msg::Apply(Box::new(move |gui| then(gui, r))));
            wake(&repaint);
        });
    }

    fn info(&mut self, text: String) {
        self.lua.log.push(text.clone());
        Gui::info(self, text);
    }

    fn error(&mut self, text: String) {
        self.lua.log.push(text.clone());
        Gui::error(self, text);
    }

    fn lua_error(&mut self, context: &str, e: &mlua::Error) {
        Gui::lua_error(self, context, e);
    }

    fn settings(&self) -> &Json {
        &self.settings
    }

    fn target_chat(&self) -> Option<usize> {
        Some(self.current)
    }

    fn prompt_text(&self) -> String {
        self.expanded()
    }

    fn prompt_images(&self) -> Vec<ImageAttachment> {
        self.images.clone()
    }

    fn set_prompt_images(&mut self, images: Vec<ImageAttachment>) -> Result<(), String> {
        if images.len() > bone_media::MAX_IMAGES {
            return Err("a message can contain at most 8 images".into());
        }
        if images.iter().map(|i| i.bytes).sum::<u64>() > bone_media::MAX_MESSAGE_IMAGE_BYTES {
            return Err("message images exceed the 40 MiB limit".into());
        }
        if images.iter().any(|i| i.data.is_some()) {
            return Err("use attachment/upload before attaching image references".into());
        }
        self.images = images;
        Gui::prompt_changed(self);
        Ok(())
    }

    fn focused_panel(&self) -> Option<String> {
        bone_ui::lua::panels::focused(&self.lua.panels, &self.lua.panel_focus).map(|p| p.id.clone())
    }

    fn set_prompt_text(&mut self, text: &str) {
        self.draft = text.to_owned();
        self.prompt.pastes.clear();
        self.cursor_to_end();
        Gui::prompt_changed(self);
    }

    /// The text box keeps its own cursor; this reports the end of the text.
    fn prompt_info(&mut self, lua: &Lua) -> mlua::Result<Table> {
        let t = lua.create_table()?;
        let lines: Vec<String> = self.draft.split('\n').map(str::to_owned).collect();
        let buf = self.buffer();
        t.set("text", self.draft.as_str())?;
        t.set(
            "images",
            bone_lua::to_lua(lua, &serde_json::json!(self.images))?,
        )?;
        t.set("lines", lines)?;
        t.set("cursor", crate::prompt::pos_out(lua, buf.cursor())?)?;
        t.set("history", self.browsing_history())?;
        if let Some((a, b)) = buf.selection() {
            let sel = lua.create_table()?;
            sel.set("start", crate::prompt::pos_out(lua, a)?)?;
            sel.set("end", crate::prompt::pos_out(lua, b)?)?;
            sel.set("text", buf.range_text(a, b))?;
            t.set("selection", sel)?;
        }
        Ok(t)
    }

    fn prompt_empty(&self) -> bool {
        self.draft.is_empty() && self.images.is_empty() && self.image_reads == 0
    }

    fn history_add(&mut self, text: &str) {
        self.remember(text, &[]);
    }

    fn process_list(&self) -> Vec<ProcessSnapshot> {
        self.processes.sorted()
    }

    fn processes(&mut self) -> &mut bone_ui::lua::processes::Processes {
        &mut self.processes
    }

    fn chat_view(&self) -> Json {
        self.chat_view_json()
    }

    fn clipboard_copy(&mut self, text: String) {
        match self.repaint.get() {
            Some(ctx) => ctx.copy_text(text),
            None => self.lua.copied = Some(text),
        }
    }

    /// The GUI's checks: what draws it, its connection, Lua, plugins, and
    /// its font.
    fn frontend_health(&self) -> Vec<Json> {
        let item = |name: &str, status: &str, message: String| serde_json::json!({ "name": name, "status": status, "message": message });
        let mut out = Vec::new();
        out.push(match &self.renderer {
            Some(r) if r.contains("(Cpu)") || r.to_lowercase().contains("llvmpipe") => item(
                "renderer",
                "warn",
                format!("{r}: drawing in software, no GPU"),
            ),
            Some(r) => item("renderer", "ok", r.clone()),
            None => item("renderer", "ok", "headless (no window)".into()),
        });
        out.push(match (&self.opts.reconnect, self.closed) {
            (_, true) => item(
                "connection",
                "error",
                "the server closed the connection".into(),
            ),
            (Some(path), _) => item(
                "connection",
                "ok",
                format!("{} (reconnects if it goes away)", path.display()),
            ),
            (None, _) => item("connection", "ok", "the core runs in this process".into()),
        });
        out.push(match self.lua.lua_errors.last() {
            Some(e) => item("lua", "warn", format!("last error: {e}")),
            None => item("lua", "ok", "no errors".into()),
        });
        let failed: Vec<String> = (self.lua.plugins.iter())
            .filter_map(|p| p.error.as_ref().map(|e| format!("{}: {e}", p.name)))
            .collect();
        let loaded = self.lua.plugins.iter().filter(|p| p.loaded).count();
        out.push(if failed.is_empty() {
            item("plugins", "ok", format!("{loaded} loaded"))
        } else {
            item("plugins", "warn", failed.join("\n"))
        });
        let font = match self.lua.fonts.as_ref() {
            Some((file, size)) if !file.is_empty() => format!("{file}, {size} pt"),
            Some((_, size)) => format!("built-in monospace, {size} pt"),
            None => "not applied yet".into(),
        };
        out.push(item("font", "ok", font));
        out
    }

    fn refresh_ui(&mut self) {
        self.lua.views_rev += 1;
        self.lua.ui_broken.clear();
        self.changed();
    }

    fn has_lua(&self) -> bool {
        self.lua.state.is_some()
    }

    fn ui_broken(&mut self) -> &mut HashSet<String> {
        &mut self.lua.ui_broken
    }

    fn generation(&self) -> (u64, u64) {
        (self.lua.views_rev, self.lua.opts_rev)
    }

    fn chat_expiry(&mut self) -> &mut Option<Instant> {
        &mut self.lua.chat_expiry
    }

    fn ui_expiry(&mut self) -> &mut Option<Instant> {
        &mut self.lua.ui_expiry
    }

    fn lua(&self) -> Option<Lua> {
        self.lua.state.clone()
    }

    fn spinner(&mut self) -> &mut bone_ui::model::Spinner {
        &mut self.lua.spinner
    }

    /// The GUI runs no `bone.job` jobs yet.
    fn jobs_running(&self) -> usize {
        self.lua.jobs.running()
    }

    fn jobs(&mut self) -> &mut bone_ui::jobs::Jobs {
        &mut self.lua.jobs
    }

    fn job_sink(&self) -> bone_ui::jobs::Sink {
        let (tx, repaint) = (self.tx.clone(), self.repaint.clone());
        std::sync::Arc::new(move |id, ev| {
            let apply = move |gui: &mut Gui| bone_ui::lua::jobs::event(gui, id, ev);
            if tx.send(crate::Msg::Apply(Box::new(apply))).is_ok() {
                crate::wake(&repaint);
            }
        })
    }

    fn screen_cols(&self) -> u16 {
        self.lua.screen_cols
    }

    /// The prompt has the keyboard unless a question or a focused window
    /// is open.
    fn prompt_focused(&self, buf: usize) -> bool {
        buf == self.current
            && self.asks.is_empty()
            && bone_ui::popup::focused(&self.lua.popups).is_none()
            && self.focused_panel().is_none()
    }

    fn geometry(&self) -> bone_ui::lua::Geometry {
        self.lua.geometry.clone()
    }

    fn popups(&mut self) -> &mut Vec<bone_ui::popup::Popup> {
        &mut self.lua.popups
    }

    fn chat_at(&self, x: u16, y: u16) -> Option<(usize, usize)> {
        let (_, r) = (self.lua.geometry.leaves.iter()).find(|(n, _)| n == "chat")?;
        if !r.contains(x, y) {
            return None;
        }
        let row = self.lua.chat_top + (y - r.y) as usize;
        self.chats[self.current].item_at_row(row)
    }

    fn key_state(&mut self) -> &mut bone_ui::lua::input::KeyState {
        &mut self.lua.input
    }

    fn key_starting(&mut self) {
        self.changed();
    }

    fn dismiss_message(&mut self) {
        self.status = None;
    }

    fn prompt_changed(&mut self) {
        Gui::prompt_changed(self);
    }

    fn type_key(&mut self, ctx: &Context, key: Key) {
        Gui::type_key(self, ctx, key);
    }

    fn interrupt(&mut self) {
        Gui::interrupt(self);
    }

    fn builtin_action(&mut self, b: bone_ui::keymap::Builtin) {
        Gui::builtin_action(self, b);
    }

    fn plugin_store(&mut self) -> bone_ui::lua::plugins::PluginStore<'_> {
        let l = &mut self.lua;
        bone_ui::lua::plugins::PluginStore {
            plugins: &mut l.plugins,
            shutdown_hooks: &mut l.shutdown_hooks,
        }
    }

    fn config_files(&self) -> &'static [&'static str] {
        &CONFIG_FILES
    }

    fn side(&self) -> bone_lua::Side {
        bone_lua::Side::Gui
    }

    fn library_changed(&mut self, prev: String) {
        self.library_prev = Some(prev);
        self.changed();
    }

    fn cwd(&self) -> &str {
        &self.opts.cwd
    }

    fn panel_store(&mut self) -> bone_ui::lua::panels::PanelStore<'_> {
        let l = &mut self.lua;
        bone_ui::lua::panels::PanelStore {
            panels: &mut l.panels,
            focus: &mut l.panel_focus,
        }
    }
}

/// Where the GUI's Lua comes from, as a hash of paths and metadata, to
/// notice edits by polling. Only what the GUI loads: the user's `ui.lua`
/// and `gui.lua`, modules, runtime overrides, colorschemes, the library in
/// use, and plugins.
pub fn lua_sources(dir: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut files = Vec::new();
    for name in ["ui.lua", "gui.lua", "library"] {
        files.push(dir.join(name));
    }
    for sub in ["lua", "runtime", "colors"] {
        collect_lua_files(&dir.join(sub), &mut files);
    }
    if let Some(name) = bone_lua::library(dir, Side::Gui) {
        collect_lua_files(&dir.join("libraries").join(name), &mut files);
    }
    for plugin in bone_lua::plugins(dir) {
        collect_lua_files(&plugin, &mut files);
    }
    files.sort();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for path in &files {
        if let Ok(meta) = std::fs::metadata(path) {
            path.hash(&mut h);
            meta.len().hash(&mut h);
            meta.modified().ok().hash(&mut h);
        }
    }
    h.finish()
}

/// Every `*.lua` under `dir`, skipping hidden folders.
fn collect_lua_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if !entry.file_name().to_string_lossy().starts_with('.') {
                collect_lua_files(&path, out);
            }
        } else if path.extension().is_some_and(|e| e == "lua") {
            out.push(path);
        }
    }
}

/// Polls the config for edits, reporting one only once it has stayed the
/// same for a whole poll (a file caught halfway through being written, or a
/// save touching several files, is not loaded).
pub struct Watcher {
    dir: std::path::PathBuf,
    state: u64,
    pending: Option<u64>,
}

impl Watcher {
    pub fn new(dir: std::path::PathBuf) -> Self {
        Watcher {
            state: lua_sources(&dir),
            dir,
            pending: None,
        }
    }

    /// How often to poll.
    pub const EVERY: std::time::Duration = std::time::Duration::from_millis(500);

    /// Poll (every [`Self::EVERY`]); true when an edit has settled.
    pub fn poll(&mut self) -> bool {
        let next = lua_sources(&self.dir);
        if next == self.state {
            self.pending = None;
            return false;
        }
        if self.pending != Some(next) {
            self.pending = Some(next);
            return false;
        }
        self.state = next;
        self.pending = None;
        true
    }
}

//! Editor state and everything that changes it: keys, actions, server events
//! and request replies. Drawing lives in `render`.
//!
//! One view: the current chat above, the prompt below. You always type into
//! the prompt; `/` starts a command. Popups (from Lua, or the session picker)
//! take the keyboard while they are open; a Lua panel takes it while focused.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use bone_client::{Client, ClientError, Event as ServerEvent};
use bone_proto::Method;
use bone_proto::methods::*;
use bone_proto::types::ImageAttachment;
use mlua::Lua;
use ratatui::layout::{Position, Rect};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::chat::{ChatBuffer, RunningTurn};
use crate::composer::{Composer, Draft};
use crate::editor::TextBuffer;
use crate::keymap::{Builtin, Context, Keymaps};
use crate::keys::Key;
use crate::layout::{BufferId, Placed, Window, WindowId};
use crate::lua::{Autocmd, UserCommand};
use crate::options::{DynamicOption, Options};
use crate::panel::Panel;
use crate::theme::Theme;
use bone_ui::lua::input::{self, KeyState};

/// A second ctrl+c within this long quits.
const QUIT_WINDOW: Duration = Duration::from_secs(2);

pub const CHAT_WIN: WindowId = 0;
pub const PROMPT_WIN: WindowId = 1;

pub use bone_ui::popup::Popup;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Error,
}

/// What `prompt/changed` reports: text, cursor and selection.
type PromptState = (
    String,
    crate::editor::Pos,
    Option<(crate::editor::Pos, crate::editor::Pos)>,
    Vec<ImageAttachment>,
);

/// The Lua state and everything it made, kept aside during a reload (see
/// [`App::swap_lua_side`]). The default has no Lua state and nothing made
/// by Lua, as before `lua::init`.
#[derive(Default)]
pub(crate) struct LuaSide {
    lua: Option<Lua>,
    keymaps: Keymaps,
    input: KeyState,
    dynamic_options: HashMap<String, DynamicOption>,
    user_commands: HashMap<String, UserCommand>,
    autocmds: Vec<Autocmd>,
    owner: Option<String>,
    callback_owner: HashMap<u64, String>,
    owned: HashMap<String, Vec<crate::plugins::Owned>>,
    keymap_owner: HashMap<(Context, Vec<Key>), String>,
    plugins: Vec<crate::plugins::Plugin>,
    shutdown_hooks: Vec<(Option<String>, u64)>,
    popups: Vec<Popup>,
    panels: Vec<Panel>,
    panel_focus: Option<String>,
    jobs: bone_ui::jobs::Jobs,
    theme: Theme,
    colors_name: Option<String>,
    ui_broken: HashSet<String>,
    lua_errors: Vec<String>,
    spinner: crate::ui::Spinner,
}

/// Work to apply on the UI thread, e.g. a request reply.
pub struct AppEvent(pub(crate) Box<dyn FnOnce(&mut App) + Send>);

pub struct App {
    client: Arc<Client>,
    asks: HashMap<u64, AskRequestedParams>,
    tx: mpsc::UnboundedSender<AppEvent>,
    pub cwd: String,
    pub chats: Vec<ChatBuffer>,
    /// The chat on screen.
    pub current: BufferId,
    pub prompt: TextBuffer,
    pub windows: HashMap<WindowId, Window>,
    /// Scroll position of every other chat (`chat:<id>` layout leaves, and
    /// where a chat was when it is shown again).
    pub chat_views: HashMap<BufferId, Window>,
    /// The chat a view is drawing, which chat queries default to.
    pub rendering: Option<BufferId>,
    /// Column separators from last frame: their cells and the layout path
    /// (1-based) to the column after each (`bone.ui.geometry`).
    pub seps: Vec<(Rect, Vec<usize>)>,
    /// Where the chat and prompt were drawn last frame.
    pub placed: HashMap<WindowId, Placed>,
    /// Every layout leaf (regions and the built-in ones) and where it was
    /// drawn last, for mouse events.
    pub leaves: Vec<(String, ratatui::layout::Rect)>,
    pub screen: Rect,
    pub keymaps: Keymaps,
    /// Where the keyboard is and the key sequence in progress.
    pub input: KeyState,
    pub message: Option<(String, Level)>,
    /// Every message shown (`bone.api.log`).
    pub log: Vec<String>,
    pub options: Options,
    pub dynamic_options: HashMap<String, DynamicOption>,
    pub opts_rev: u64,
    /// Bumped when Lua changes views; part of every render cache key.
    pub views_rev: u64,
    /// While a chat view draws: the chat queries it made, and when it asked
    /// to be drawn again (`bone.chat.refresh_in`).
    pub render_deps: Option<Vec<crate::data::Dep>>,
    pub render_expires: Option<std::time::Instant>,
    /// When the chat on screen next needs drawing for a view's timer.
    pub chat_expiry: Option<std::time::Instant>,
    /// A UI redraw requested without invalidating cached chat views.
    pub ui_expiry: Option<std::time::Instant>,
    /// Numbers the items Lua adds to chats.
    pub lua_item_seq: u64,
    pub spinner: crate::ui::Spinner,
    /// The terminal title last set (`bone.ui.title`).
    pub title: Option<String>,
    pub theme: Theme,
    pub colors_name: Option<String>,
    /// `bone.ui` functions that errored; skipped until restart.
    pub ui_broken: HashSet<String>,
    pub popups: Vec<Popup>,
    /// Docked Lua panels (`bone.ui.panel`), in opening order.
    pub panels: Vec<Panel>,
    /// The panel with the keyboard (see `focused_panel`).
    pub panel_focus: Option<String>,
    /// Streaming jobs from Lua (`bone.job`).
    pub jobs: bone_ui::jobs::Jobs,
    /// Core-managed shell processes of the chat on screen.
    pub processes: bone_ui::lua::processes::Processes,
    /// Text highlighted with the mouse.
    pub selection: Option<crate::selection::Selection>,
    /// Text to put on the system clipboard after the next draw.
    pub clipboard: Option<String>,
    composer: Composer,
    pub(crate) clipboard_executable: Option<PathBuf>,
    last_prompt_state: PromptState,
    pub(crate) quit_armed: Option<Instant>,
    pub dirty: bool,
    /// Draw once more after this frame: something drawn earlier in it
    /// (a region) saw state the frame then changed.
    pub redraw: bool,
    pub quit: Option<Option<String>>,
    pub lua: Option<Lua>,
    pub config_dir: Option<PathBuf>,
    pub next_callback: u64,
    pub user_commands: HashMap<String, UserCommand>,
    pub autocmds: Vec<Autocmd>,
    /// The plugin whose code is running (loading, or one of its callbacks);
    /// `None` for the runtime and the user's config.
    pub owner: Option<String>,
    /// The plugin each stored callback belongs to.
    pub callback_owner: HashMap<u64, String>,
    /// What each plugin created (see `plugins.rs`).
    pub owned: HashMap<String, Vec<crate::plugins::Owned>>,
    /// Which plugin last mapped each key.
    pub keymap_owner: HashMap<(Context, Vec<Key>), String>,
    pub plugins: Vec<crate::plugins::Plugin>,
    /// `bone.plugin.on_shutdown` callbacks, by owner.
    pub shutdown_hooks: Vec<(Option<String>, u64)>,
    /// The saved settings (settings.json, via the core), kept current by
    /// `settings/changed`.
    pub settings: serde_json::Value,
    /// The last Lua errors in full (tracebacks included), for /health.
    pub lua_errors: Vec<String>,
    /// Option values kept across a reload, for options defined again.
    pub carried_options: HashMap<String, crate::options::DynamicValue>,
    /// While the Lua config loads (at startup or on a reload): each Lua
    /// error's context and short message.
    pub load_errors: Option<Vec<(String, String)>>,
    /// Where the last accepted load had errors. A reload that fails only
    /// where this one did is still taken, so an old error cannot pin the
    /// configuration.
    pub load_failures: Vec<String>,
    /// Reload the core when its Lua files change (see `RunOptions`).
    pub reload_core: bool,
    /// Reload the TUI's Lua from the main loop (a library was chosen).
    pub reload_requested: bool,
    /// Lua loaded or unloaded a plugin itself: the watcher takes the files
    /// as they are instead of reloading (which would drop pending callbacks).
    pub rewatch: bool,
    /// The library before `/tui` switched, chosen again if the switch fails.
    pub library_prev: Option<String>,
}

impl App {
    /// `config_dir` is where runtime overrides and `tui.lua` live; `None`
    /// uses only the embedded runtime.
    pub fn new(
        client: Arc<Client>,
        tx: mpsc::UnboundedSender<AppEvent>,
        cwd: String,
        config_dir: Option<PathBuf>,
    ) -> Self {
        let window = Window {
            top: 0,
            follow: true,
        };
        let mut app = App {
            client,
            asks: HashMap::new(),
            tx,
            cwd,
            chats: vec![ChatBuffer::new(None)],
            current: 0,
            prompt: TextBuffer::default(),
            windows: HashMap::from([(CHAT_WIN, window.clone()), (PROMPT_WIN, window)]),
            chat_views: HashMap::new(),
            rendering: None,
            seps: Vec::new(),
            placed: HashMap::new(),
            leaves: Vec::new(),
            screen: Rect::default(),
            keymaps: Keymaps::default(),
            input: KeyState::default(),
            message: None,
            log: Vec::new(),
            options: Options::default(),
            dynamic_options: HashMap::new(),
            opts_rev: 0,
            views_rev: 0,
            render_deps: None,
            render_expires: None,
            chat_expiry: None,
            ui_expiry: None,
            lua_item_seq: 0,
            spinner: Default::default(),
            title: None,
            theme: Theme::default(),
            colors_name: None,
            ui_broken: HashSet::new(),
            popups: Vec::new(),
            panels: Vec::new(),
            panel_focus: None,
            jobs: Default::default(),
            processes: Default::default(),
            selection: None,
            clipboard: None,
            composer: Composer::default(),
            clipboard_executable: None,
            last_prompt_state: (String::new(), (0, 0), None, Vec::new()),
            quit_armed: None,
            dirty: true,
            redraw: false,
            quit: None,
            lua: None,
            config_dir: config_dir.clone(),
            next_callback: 0,
            user_commands: HashMap::new(),
            autocmds: Vec::new(),
            owner: None,
            callback_owner: HashMap::new(),
            owned: HashMap::new(),
            keymap_owner: HashMap::new(),
            plugins: Vec::new(),
            shutdown_hooks: Vec::new(),
            carried_options: HashMap::new(),
            lua_errors: Vec::new(),
            settings: serde_json::Value::Null,
            load_errors: Some(Vec::new()),
            load_failures: Vec::new(),
            reload_core: false,
            reload_requested: false,
            rewatch: false,
            library_prev: None,
        };
        crate::lua::init(&mut app, config_dir);
        app.dirty = true;
        app
    }

    /// Put `side` in place of the current Lua state and everything Lua made
    /// (keymaps, commands, events, popups, panels, jobs, options, plugin
    /// records, colors and UI state), returning what was there. Chats (Lua's
    /// items in them too), the prompt, history, built-in options and the
    /// session are not part of it. A reload builds the new side while the old
    /// one waits here, so a failed reload can put the old one back.
    pub(crate) fn swap_lua_side(&mut self, mut side: LuaSide) -> LuaSide {
        use std::mem::swap;
        swap(&mut self.lua, &mut side.lua);
        swap(&mut self.keymaps, &mut side.keymaps);
        swap(&mut self.input, &mut side.input);
        swap(&mut self.dynamic_options, &mut side.dynamic_options);
        swap(&mut self.user_commands, &mut side.user_commands);
        swap(&mut self.autocmds, &mut side.autocmds);
        swap(&mut self.owner, &mut side.owner);
        swap(&mut self.callback_owner, &mut side.callback_owner);
        swap(&mut self.owned, &mut side.owned);
        swap(&mut self.keymap_owner, &mut side.keymap_owner);
        swap(&mut self.plugins, &mut side.plugins);
        swap(&mut self.shutdown_hooks, &mut side.shutdown_hooks);
        swap(&mut self.popups, &mut side.popups);
        swap(&mut self.panels, &mut side.panels);
        swap(&mut self.panel_focus, &mut side.panel_focus);
        swap(&mut self.jobs, &mut side.jobs);
        swap(&mut self.theme, &mut side.theme);
        swap(&mut self.colors_name, &mut side.colors_name);
        swap(&mut self.ui_broken, &mut side.ui_broken);
        swap(&mut self.lua_errors, &mut side.lua_errors);
        swap(&mut self.spinner, &mut side.spinner);
        self.views_rev += 1;
        self.opts_rev += 1;
        self.dirty = true;
        side
    }

    /// Drop a Lua side that is not current: cancel its jobs without calling
    /// back, then let it go.
    pub(crate) fn retire_lua_side(&mut self, side: LuaSide) {
        let current = self.swap_lua_side(side);
        let jobs: Vec<u64> = self.jobs.list.iter().map(|j| j.id).collect();
        for id in jobs {
            bone_ui::lua::jobs::cancel(self, id);
            self.jobs.forget_callbacks(id);
        }
        drop(self.swap_lua_side(current));
    }

    // ---- requests ------------------------------------------------------------

    /// Send a request; `then` runs on the UI thread with the reply.
    pub fn request<M: Method>(
        &self,
        params: M::Params,
        then: impl FnOnce(&mut App, Result<M::Result, ClientError>) + Send + 'static,
    ) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let r = client.request::<M>(params).await;
            let _ = tx.send(AppEvent(Box::new(move |app| then(app, r))));
        });
    }

    /// Untyped request, for Lua.
    pub fn request_raw(
        &self,
        method: String,
        params: serde_json::Value,
        then: impl FnOnce(&mut App, Result<serde_json::Value, ClientError>) + Send + 'static,
    ) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let r = client.request_raw(&method, params).await;
            let _ = tx.send(AppEvent(Box::new(move |app| then(app, r))));
        });
    }

    /// Run `fut` in the background, then `then` on the UI thread.
    pub fn spawn<T: Send + 'static>(
        &self,
        fut: impl std::future::Future<Output = T> + Send + 'static,
        then: impl FnOnce(&mut App, T) + Send + 'static,
    ) {
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let r = fut.await;
            let _ = tx.send(AppEvent(Box::new(move |app| then(app, r))));
        });
    }

    /// Where background tasks send work for the UI thread.
    pub fn event_sender(&self) -> mpsc::UnboundedSender<AppEvent> {
        self.tx.clone()
    }

    pub fn apply(&mut self, ev: AppEvent) {
        (ev.0)(self);
        self.dirty = true;
    }

    pub fn info(&mut self, text: impl Into<String>) {
        let text = text.into();
        self.log.push(text.clone());
        self.message = Some((text, Level::Info));
        self.dirty = true;
    }

    pub fn error(&mut self, text: impl Into<String>) {
        let text = text.into();
        self.log.push(text.clone());
        self.message = Some((text, Level::Error));
        self.dirty = true;
    }

    // ---- chats ---------------------------------------------------------------

    pub fn chat_mut(&mut self, id: BufferId) -> Option<&mut ChatBuffer> {
        self.chats.get_mut(id)
    }

    /// The chat on screen, which prompts are sent to.
    pub fn target_chat(&self) -> Option<BufferId> {
        Some(self.current)
    }

    pub fn current_session_id(&self) -> Option<String> {
        self.chats[self.current].session_id().map(str::to_owned)
    }

    /// A chat by its session id, or by its number (`bone.chat.session().chat`),
    /// which a new chat has before its session.
    pub(crate) fn chat_by_session(&self, id: &str) -> Option<BufferId> {
        bone_ui::chat::find_chat(&self.chats, id)
    }

    pub fn show_chat(&mut self, id: BufferId) {
        if id != self.current {
            self.composer.drafts.insert(self.current, self.draft());
            let draft = self.composer.drafts.remove(&id).unwrap_or_else(|| {
                self.composer.next_epoch += 1;
                Draft {
                    epoch: self.composer.next_epoch,
                    ..Default::default()
                }
            });
            self.apply_draft(draft);
            self.composer.history_pos = None;
            self.composer.saved = None;
        }
        // Each chat keeps its scroll position (render.rs drops a hidden
        // chat's rendered lines).
        let w = if id == self.current {
            Window::default()
        } else {
            let old = self.windows.get(&CHAT_WIN).cloned().unwrap_or_default();
            self.chat_views.insert(self.current, old);
            self.chat_views.remove(&id).unwrap_or_default()
        };
        self.windows.insert(CHAT_WIN, w);
        self.current = id;
        // The tray's shells are the chat on screen's.
        match self.chats[id].session_id().map(str::to_owned) {
            Some(session_id) => self.request_processes(session_id),
            None => self.clear_processes(),
        }
        let session_id = self.chats[id].session_id().map(str::to_owned);
        self.fire(
            "chat/shown",
            serde_json::json!({ "session_id": session_id, "chat": id }),
        );
        self.dirty = true;
    }

    /// A fresh session (created on the first message).
    pub fn new_session(&mut self) {
        let c = &self.chats[self.current];
        if c.session.is_none() && c.entries.is_empty() && !c.starting {
            return;
        }
        self.chats.push(ChatBuffer::new(None));
        self.show_chat(self.chats.len() - 1);
    }

    pub fn prompt_text(&self) -> String {
        let mut text = self.prompt.text();
        for (token, content) in &self.composer.pastes {
            text = text.replace(token, content);
        }
        text
    }

    fn expand_pastes(&mut self) {
        if !self.composer.pastes.is_empty() {
            self.prompt.set_text(&self.prompt_text());
            self.composer.pastes.clear();
        }
    }

    fn draft(&self) -> Draft {
        self.composer.snapshot(&self.prompt)
    }

    /// The prompt's text for chat `buf`: the one being edited, or its draft.
    pub fn prompt_of(&self, buf: BufferId) -> TextBuffer {
        if buf == self.current {
            return self.prompt.clone();
        }
        let mut t = TextBuffer::default();
        t.set_text(self.composer.drafts.get(&buf).map_or("", |d| &d.text));
        t
    }

    fn apply_draft(&mut self, draft: Draft) {
        self.composer.apply(&mut self.prompt, draft);
        self.dirty = true;
    }

    pub fn prompt_images(&self) -> &[ImageAttachment] {
        &self.composer.images
    }

    pub fn draft_empty(&self) -> bool {
        self.prompt.is_empty()
            && self.composer.images.is_empty()
            && !self.composer.reading(self.current)
    }

    fn clear_prompt(&mut self) {
        self.prompt.clear();
        self.composer.pastes.clear();
        self.composer.images.clear();
        self.composer.invalidate();
    }

    pub fn set_prompt_images(&mut self, images: Vec<ImageAttachment>) -> Result<(), String> {
        if images.len() > bone_media::MAX_IMAGES {
            return Err("a message can contain at most 8 images".into());
        }
        if images.iter().map(|i| i.bytes).sum::<u64>() > bone_media::MAX_MESSAGE_IMAGE_BYTES {
            return Err("message images exceed the 40 MiB limit".into());
        }
        if images.iter().any(|i| i.data.is_some()) {
            return Err("use attachment/upload before attaching image references".into());
        }
        self.composer.images = images;
        self.emit_prompt_changed();
        self.dirty = true;
        Ok(())
    }

    pub fn remove_image(&mut self, index: usize) -> Result<(), String> {
        if index == 0 || index > self.composer.images.len() {
            return Err("image index is out of range".into());
        }
        self.composer.images.remove(index - 1);
        self.emit_prompt_changed();
        self.dirty = true;
        Ok(())
    }

    pub fn paste_clipboard(&mut self) {
        self.read_image(crate::clipboard::Input::Clipboard);
    }

    pub fn attach_file(&mut self, path: PathBuf) {
        let path = if path.is_absolute() {
            path
        } else {
            let cwd = self.chats[self.current]
                .session
                .as_ref()
                .map(|s| s.cwd.as_str())
                .unwrap_or(&self.cwd);
            PathBuf::from(cwd).join(path)
        };
        self.read_image(crate::clipboard::Input::File(path));
    }

    fn read_image(&mut self, input: crate::clipboard::Input) {
        if self.focused_popup().is_some() || self.focused_panel().is_some() {
            return self.error("focus the prompt to paste an image");
        }
        let (buf, epoch) = (self.current, self.composer.epoch);
        let pending = self
            .composer
            .pending
            .get(&(buf, epoch))
            .copied()
            .unwrap_or(0);
        if pending >= bone_media::MAX_IMAGES {
            return self.error("too many clipboard/image reads are pending; try again in a moment");
        }
        // A clipboard may hold text, which must still paste at the image limit.
        if matches!(input, crate::clipboard::Input::File(_))
            && self.composer.images.len() + pending >= bone_media::MAX_IMAGES
        {
            return self.error("a message can contain at most 8 images");
        }
        *self.composer.pending.entry((buf, epoch)).or_default() += 1;
        let tx = self.tx.clone();
        let executable = self.clipboard_executable.clone();
        tokio::spawn(async move {
            let result = crate::clipboard::read(input, executable).await;
            let _ = tx.send(AppEvent(Box::new(move |app| match result {
                Ok(crate::clipboard::Content::Image { bytes, name }) => {
                    let params = AttachmentUploadParams {
                        data: base64::engine::general_purpose::STANDARD.encode(bytes),
                        name,
                    };
                    app.request::<AttachmentUpload>(params, move |app, result| {
                        app.composer.finish_read(buf, epoch);
                        match result {
                            Ok(image) => app.deliver_image(buf, epoch, image),
                            Err(e) => app.error(format!("cannot attach image: {e}")),
                        }
                    });
                }
                Ok(crate::clipboard::Content::Text(text)) => {
                    app.composer.finish_read(buf, epoch);
                    if buf == app.current && epoch == app.composer.epoch {
                        app.paste(&text);
                    } else if let Some(draft) = app
                        .composer
                        .drafts
                        .get_mut(&buf)
                        .filter(|d| d.epoch == epoch)
                    {
                        draft.text.push_str(&bone_ui::composer::clean_paste(&text));
                    }
                }
                Err(e) => {
                    app.composer.finish_read(buf, epoch);
                    app.error(e);
                }
            })));
        });
        self.info("reading clipboard/image…");
    }

    fn deliver_image(&mut self, buf: BufferId, epoch: u64, image: ImageAttachment) {
        match self
            .composer
            .attach(self.current, buf, epoch, image.clone())
        {
            Ok(true) => {
                self.fire("image/attached", serde_json::json!({ "image": image }));
                self.emit_prompt_changed();
                self.info("image attached · enter sends · /detach removes");
            }
            Ok(false) => {}
            Err(e) => self.error(e),
        }
        self.dirty = true;
    }

    fn restore_draft(&mut self, buf: BufferId, mut failed: Draft) {
        if buf == self.current {
            self.expand_pastes();
            let current = self.prompt.text();
            if !current.is_empty() {
                failed.text = if failed.text.is_empty() {
                    current
                } else {
                    format!("{current}\n{}", failed.text)
                };
            }
            failed.images.splice(0..0, self.composer.images.clone());
            failed.epoch = self.composer.epoch;
            self.apply_draft(failed);
            self.emit_prompt_changed();
        } else {
            let draft = self.composer.drafts.entry(buf).or_default();
            if !draft.text.is_empty() && !failed.text.is_empty() {
                draft.text.push('\n');
            }
            draft.text.push_str(&failed.text);
            draft.images.extend(failed.images);
        }
    }

    pub fn prompt_history_active(&self) -> bool {
        self.composer.history_pos.is_some()
    }

    pub fn set_prompt_text(&mut self, text: &str) {
        self.composer.pastes.clear();
        self.prompt.set_text(text);
        self.emit_prompt_changed();
        self.dirty = true;
    }

    pub(crate) fn emit_prompt_changed(&mut self) {
        let state = (
            self.prompt.text(),
            self.prompt.cursor(),
            self.prompt.selection(),
            self.composer.images.clone(),
        );
        if self.last_prompt_state == state {
            return;
        }
        self.last_prompt_state = state.clone();
        let pos = |(row, col): crate::editor::Pos| serde_json::json!({ "row": row, "col": col });
        self.fire(
            "prompt/changed",
            serde_json::json!({
                "text": state.0,
                "images": state.3,
                "cursor": pos(state.1),
                "selection": state.2.map(|(a, b)| serde_json::json!({ "start": pos(a), "end": pos(b) })),
            }),
        );
    }

    pub fn resize(&mut self, width: u16, height: u16) {
        if self.screen.width == width && self.screen.height == height {
            self.dirty = true;
            return;
        }
        self.screen.width = width;
        self.screen.height = height;
        self.fire(
            "resize",
            serde_json::json!({ "width": width, "height": height }),
        );
        self.dirty = true;
    }

    // ---- keys ----------------------------------------------------------------

    /// Which keymaps apply right now: a focused popup's, then a focused
    /// panel's (`panel` or its own context), then a named context selected
    /// by Lua, then `main`.
    pub fn context(&self) -> Context {
        input::context_of(&self.popups, &self.panels, &self.panel_focus, &self.input)
    }

    pub fn key_sequence_deadline(&self) -> Option<Instant> {
        (self.input.pending_since)
            .map(|since| since + Duration::from_millis(self.options.timeoutlen))
    }

    /// Expire a pending key sequence when its current `timeoutlen` deadline
    /// has passed. This is called by the terminal loop even when nothing is
    /// being redrawn.
    pub fn expire_key_sequence(&mut self) -> bool {
        input::expire_key_sequence(self)
    }

    pub fn handle_key(&mut self, key: Key) {
        input::handle_key(self, key);
    }

    pub(crate) fn type_key(&mut self, ctx: &Context, key: Key) {
        let Some(c) = key.text() else { return };
        // Text goes to the prompt only while it has the keyboard.
        match ctx {
            Context::Main | Context::Named(_) if self.focused_panel().is_none() => {
                self.composer.history_pos = None;
                self.prompt.insert_char(c);
            }
            _ => {}
        }
    }

    pub fn paste(&mut self, text: &str) {
        self.dirty = true;
        self.flush_pending();
        let context = self.context().name().to_owned();
        self.fire(
            "paste",
            serde_json::json!({ "text": text, "context": context }),
        );
        match self.context() {
            Context::Main | Context::Named(_) if self.focused_panel().is_none() => {
                self.composer.history_pos = None;
                let text = bone_ui::composer::clean_paste(text);
                if text.chars().count() > bone_ui::composer::PASTE_PLACEHOLDER_THRESHOLD {
                    let token =
                        bone_ui::composer::paste_token(self.composer.pastes.len() + 1, &text);
                    self.prompt.insert_str(&token);
                    self.composer.pastes.push((token, text));
                } else {
                    self.prompt.insert_str(&text);
                }
            }
            _ => {}
        }
        self.emit_prompt_changed();
    }

    pub(crate) fn flush_pending(&mut self) {
        input::flush_pending(self);
    }

    /// What `b` does to a selection in the prompt: deleting removes it,
    /// moving drops it. True when that was all.
    pub(crate) fn builtin_on_selection(&mut self, b: Builtin) -> bool {
        use Builtin::*;
        if self.prompt.selection().is_some() {
            match b {
                Backspace | Delete | DeleteWord | DeleteToStart | DeleteToEnd => {
                    self.prompt.delete_selection();
                    return true;
                }
                Left | Right | Up | Down | WordLeft | WordRight | LineStart | LineEnd
                | Complete | Dismiss => self.prompt.clear_selection(),
                _ => {}
            }
        }
        false
    }

    /// What a built-in action does in the TUI, when neither a panel nor Lua
    /// took it.
    pub(crate) fn builtin_action(&mut self, b: Builtin) {
        use Builtin::*;
        match b {
            PasteClipboard => self.paste_clipboard(),
            Submit => self.submit(None),
            QueueSteer => self.submit(Some(QueueMode::Steer)),
            QueueNext => self.submit(Some(QueueMode::Next)),
            Newline => self.prompt.newline(),
            Left => self.prompt.left(),
            Right => self.prompt.right(),
            Up | Down => self.vertical(b == Up),
            WordLeft => self.prompt.word_back(),
            WordRight => self.prompt.word_forward(),
            LineStart => self.prompt.line_start(),
            LineEnd => self.prompt.line_end(),
            // A paste's placeholder goes in one backspace.
            Backspace => {
                let (row, col) = self.prompt.cursor();
                let before: String = self.prompt.lines()[row].chars().take(col).collect();
                let token = self.composer.pastes.iter().find(|p| before.ends_with(&p.0));
                for _ in 0..token.map_or(1, |p| p.0.chars().count()) {
                    self.prompt.backspace();
                }
            }
            Delete => self.prompt.delete(),
            DeleteWord => self.prompt.delete_word_back(),
            DeleteToStart => self.prompt.delete_to_line_start(),
            DeleteToEnd => self.prompt.delete_to_line_end(),
            ScrollUp => self.scroll(-3),
            ScrollDown => self.scroll(3),
            PageUp => self.scroll(-(self.page() - 2).max(1)),
            PageDown => self.scroll((self.page() - 2).max(1)),
            ScrollTop => {
                let w = self.windows.get_mut(&CHAT_WIN).unwrap();
                w.top = 0;
                w.follow = false;
            }
            ScrollBottom => self.windows.get_mut(&CHAT_WIN).unwrap().follow = true,
            // Without Lua: nothing to complete; esc clears the prompt.
            Complete => {}
            Dismiss => {
                self.clear_prompt();
                self.message = None;
            }
            Interrupt => self.interrupt(),
            Quit => self.quit = Some(None),
            QuitIfEmpty => {
                if self.draft_empty() {
                    self.quit = Some(None);
                } else {
                    self.prompt.delete();
                }
            }
            NewSession => self.new_session(),
            Sessions => self.open_picker(),
            FocusNext => input::cycle_focus(self, true),
            FocusPrev => input::cycle_focus(self, false),
            FocusPrompt => {
                let _ = self.focus_panel(None);
            }
        }
    }

    /// Remember a sent prompt or command for up/down recall.
    pub fn history_add(&mut self, text: &str) {
        let draft = Draft {
            text: text.to_owned(),
            ..Default::default()
        };
        self.history_add_draft(draft);
        self.composer.history_pos = None;
    }

    fn history_add_draft(&mut self, draft: Draft) {
        if self
            .composer
            .history
            .last()
            .is_none_or(|last| last.text != draft.text || last.images != draft.images)
        {
            self.composer.history.push(draft);
        }
        self.composer.history_pos = None;
        self.composer.saved = None;
    }

    /// Up/Down: move between prompt lines, and past the first/last line
    /// through history.
    fn vertical(&mut self, up: bool) {
        let moved = if up {
            self.prompt.up()
        } else {
            self.prompt.down()
        };
        if moved || self.composer.history.is_empty() {
            return;
        }
        let len = self.composer.history.len();
        let pos = match (self.composer.history_pos, up) {
            (None, true) => Some(len - 1),
            (None, false) => return,
            (Some(p), true) => Some(p.saturating_sub(1)),
            (Some(p), false) if p + 1 < len => Some(p + 1),
            (Some(_), false) => None,
        };
        if self.composer.history_pos.is_none() && pos.is_some() {
            self.composer.saved = Some(self.draft());
        }
        self.composer.history_pos = pos;
        let mut draft = pos
            .map(|p| self.composer.history[p].clone())
            .unwrap_or_else(|| self.composer.saved.take().unwrap_or_default());
        self.composer.invalidate();
        draft.epoch = self.composer.epoch;
        self.apply_draft(draft);
        self.emit_prompt_changed();
    }

    fn page(&self) -> i64 {
        self.placed
            .get(&CHAT_WIN)
            .map_or(10, |p| p.area.height as i64)
    }

    fn scroll(&mut self, by: i64) {
        if let Some(w) = self.windows.get_mut(&CHAT_WIN) {
            w.top = (w.top as i64 + by).max(0) as usize;
            // Render clamps and re-enables follow at the bottom.
            w.follow = false;
        }
    }

    /// ctrl+c: cancel the turn, else clear the prompt, else quit on the
    /// second press.
    pub(crate) fn interrupt(&mut self) {
        if self.chats[self.current].turn.is_some()
            && let Some(session_id) = self.chats[self.current].session_id().map(str::to_owned)
        {
            self.request::<TurnCancel>(SessionRef { session_id }, |app, r| {
                if let Err(e) = r {
                    app.error(format!("cancel failed: {e}"));
                }
            });
            return self.info("cancelling…");
        }
        if !self.draft_empty() {
            self.clear_prompt();
            return;
        }
        if self.quit_armed.is_some_and(|t| t.elapsed() < QUIT_WINDOW) {
            self.quit = Some(None);
            return;
        }
        self.quit_armed = Some(Instant::now());
        self.info("Press ctrl+c again to quit");
    }

    // ---- turns ---------------------------------------------------------------

    /// Send the prompt: a new turn when idle; while a turn runs, queued with
    /// `mode` (default: the `queue_mode` option, else steer).
    fn submit(&mut self, mode: Option<QueueMode>) {
        if self.composer.reading(self.current) {
            return self.info("wait for the image to finish attaching before sending");
        }
        self.expand_pastes();
        let mut draft = self.draft();
        if draft.text.trim().is_empty() && draft.images.is_empty() {
            return;
        }
        match bone_ui::lua::fire_submit(self, &draft.text, &draft.images) {
            Some(text) => draft.text = text,
            None => {
                self.clear_prompt();
                self.emit_prompt_changed();
                return;
            }
        }
        if draft.text.trim().is_empty() && draft.images.is_empty() {
            return;
        }
        let buf = self.current;
        if self.chats[buf].turn.is_some()
            && let Some(session_id) = self.chats[buf].session_id().map(str::to_owned)
        {
            let mode = mode.unwrap_or_else(|| bone_ui::options::queue_mode(&self.dynamic_options));
            self.history_add_draft(draft.clone());
            self.clear_prompt();
            let params = QueueAddParams {
                session_id,
                text: draft.text.clone(),
                mode,
                images: draft.images.clone(),
            };
            self.request::<QueueAdd>(params, move |app, r| {
                if let Err(e) = r {
                    app.restore_draft(buf, draft);
                    app.error(format!("not sent ({e}); it is back in its draft"));
                }
            });
            return;
        }
        if self.chats[buf].starting {
            return self.error("A turn is still starting");
        }
        self.chats[buf].starting = true;
        self.history_add_draft(draft.clone());
        self.clear_prompt();
        match self.chats[buf].session_id() {
            Some(id) => self.start_turn(buf, id.to_owned(), draft),
            None => {
                let cwd = Some(self.cwd.clone());
                self.request::<SessionCreate>(SessionCreateParams { cwd }, move |app, r| match r {
                    Ok(info) => {
                        let id = info.session_id.clone();
                        if let Some(c) = app.chat_mut(buf) {
                            c.session = Some(info);
                        }
                        app.request_processes(id.clone());
                        app.start_turn(buf, id, draft);
                    }
                    Err(e) => {
                        app.turn_not_started(buf, draft, format!("cannot create session: {e}"))
                    }
                });
            }
        }
    }

    fn start_turn(&mut self, buf: BufferId, session_id: String, draft: Draft) {
        let params = TurnStartParams {
            session_id,
            text: draft.text.clone(),
            images: draft.images.clone(),
        };
        // Running once accepted (turn/started waits on hooks and MCP), unless it already finished.
        self.request::<TurnStart>(params, move |app, r| match r {
            Ok(r) => {
                if let Some(c) = app.chat_mut(buf).filter(|c| c.starting) {
                    c.starting = false;
                    c.turn.get_or_insert(RunningTurn {
                        turn_id: r.turn_id,
                        started: Instant::now(),
                    });
                }
            }
            Err(e) => app.turn_not_started(buf, draft, format!("cannot start turn: {e}")),
        });
    }

    fn turn_not_started(&mut self, buf: BufferId, draft: Draft, why: String) {
        if let Some(c) = self.chat_mut(buf) {
            c.starting = false;
        }
        self.restore_draft(buf, draft);
        self.error(format!("{why}; message restored to its draft"));
    }

    /// Open a session's chat, and show it unless `show` is false (it then
    /// loads in the background, for a `chat:<id>` layout leaf).
    pub fn open_session(&mut self, session_id: String, show: bool) -> BufferId {
        if let Some(buf) = self.chat_by_session(&session_id) {
            if show {
                self.show_chat(buf);
            }
            return buf;
        }
        // Reuse an untouched new chat rather than piling them up.
        let c = &self.chats[self.current];
        let buf = if show
            && c.session.is_none()
            && c.entries.is_empty()
            && !c.starting
            && self.draft_empty()
        {
            self.current
        } else {
            self.chats.push(ChatBuffer::new(None));
            self.chats.len() - 1
        };
        if show {
            self.show_chat(buf);
        }
        self.request::<SessionMessages>(
            SessionMessagesParams {
                session_id,
                tail: None,
            },
            move |app, r| {
                match r {
                    Ok(r) => {
                        let session_id = r.info.session_id.clone();
                        if let Some(c) = app.chat_mut(buf) {
                            c.load(r.info, &r.messages, r.active_turn);
                            c.set_tool_times(&r.tool_times);
                            c.set_queue(r.queue, r.queue_paused);
                        }
                        if buf == app.current {
                            app.request_processes(session_id.clone());
                            let ev = serde_json::json!({ "session_id": session_id, "chat": buf });
                            app.fire("chat/shown", ev);
                        }
                    }
                    Err(e) => {
                        if let Some(c) = app.chat_mut(buf) {
                            c.notice(format!("cannot load session: {e}"), true);
                        }
                    }
                }
                trim_heap();
            },
        );
        buf
    }

    /// The session picker is Lua: `bone.ui.sessions()` (the default is in
    /// runtime/tui/defaults.lua).
    pub fn open_picker(&mut self) {
        self.call_ui("sessions", None);
    }

    /// Call `bone.ui[name](arg)`, a Lua part of the UI (session picker,
    /// help, health).
    pub fn call_ui(&mut self, name: &str, arg: Option<&str>) {
        let r = self.with_api(|lua| {
            let ui: mlua::Table = lua.globals().get::<mlua::Table>("bone")?.get("ui")?;
            match ui.get::<Option<mlua::Function>>(name)? {
                Some(f) => f.call::<()>(arg).map(|_| true),
                None => Ok(false),
            }
        });
        match r {
            Ok(true) => {}
            Ok(false) => self.error(format!("bone.ui.{name} is not defined")),
            Err(e) => self.lua_error(&format!("bone.ui.{name}"), &e),
        }
    }

    /// The TUI's own checks for /health: terminal, mouse, clipboard, Lua.
    pub fn tui_health(&self) -> Vec<serde_json::Value> {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let item = |name: &str, status: &str, message: String| serde_json::json!({ "name": name, "status": status, "message": message });
        let on_path = |bin: &str| {
            env("PATH").is_some_and(|p| {
                p.split(':')
                    .any(|d| std::path::Path::new(d).join(bin).is_file())
            })
        };
        let mut out = Vec::new();
        let library = self.config_dir.as_deref().and_then(bone_lua::tui_library);
        out.push(item(
            "library",
            "ok",
            library.unwrap_or_else(|| "standard".into()) + " (/tui switches)",
        ));
        let term = env("TERM").unwrap_or_else(|| "unset".into());
        let tmux = env("TMUX").is_some();
        out.push(item(
            "terminal",
            "ok",
            format!("TERM={term}{}", if tmux { ", inside tmux" } else { "" }),
        ));
        out.push(item(
            "mouse",
            "ok",
            if self.options.mouse {
                "on: the wheel scrolls, dragging copies (turn it off with bone.o.mouse = false)"
                    .into()
            } else {
                "off: the terminal selects text; the wheel does not scroll".into()
            },
        ));
        let ssh = env("SSH_CONNECTION").is_some();
        let (status, route) = if tmux {
            (
                "ok",
                "tmux load-buffer -w (tmux passes it to your terminal)".to_owned(),
            )
        } else if !ssh && env("WAYLAND_DISPLAY").is_some() {
            if on_path("wl-copy") {
                ("ok", "wl-copy and OSC 52".into())
            } else {
                (
                    "warn",
                    "OSC 52 only; install wl-clipboard for wl-copy".into(),
                )
            }
        } else if !ssh && env("DISPLAY").is_some() {
            if on_path("xclip") {
                ("ok", "xclip and OSC 52".into())
            } else {
                ("warn", "OSC 52 only; install xclip".into())
            }
        } else {
            (
                "ok",
                "OSC 52 (your terminal must allow clipboard writes)".into(),
            )
        };
        out.push(item("clipboard", status, route));
        let last = self
            .lua_errors
            .last()
            .map(|e| format!("\nlast error:\n{e}"))
            .unwrap_or_default();
        if self.ui_broken.is_empty() {
            out.push(item(
                "tui lua",
                if self.lua_errors.is_empty() {
                    "ok"
                } else {
                    "warn"
                },
                format!("no errors in views, regions or windows{last}"),
            ));
        } else {
            let mut names: Vec<&String> = self.ui_broken.iter().collect();
            names.sort();
            let names: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
            out.push(item(
                "tui lua",
                "error",
                format!("switched off after errors: {}{last}", names.join(", ")),
            ));
        }
        let overrides = self
            .config_dir
            .as_deref()
            .map(bone_lua::runtime_overrides)
            .unwrap_or_default();
        if overrides.is_empty() {
            out.push(item(
                "runtime overrides",
                "ok",
                "none; the built-in runtime is used".into(),
            ));
        } else {
            let list: Vec<String> = overrides
                .iter()
                .map(|(rel, same)| {
                    if *same {
                        format!("{rel} (same as built-in)")
                    } else {
                        format!("{rel} (differs from built-in)")
                    }
                })
                .collect();
            out.push(item(
                "runtime overrides",
                "warn",
                format!(
                    "these replace built-in files and hide their updates (/runtime reset FILE goes back to the built-in one): {}",
                    list.join(", ")
                ),
            ));
        }
        out
    }

    /// Resume a session at startup: `None` for the newest one.
    pub fn resume(&mut self, id: Option<String>) {
        match id {
            Some(id) => {
                self.open_session(id, true);
            }
            None => self.request::<SessionList>(Empty {}, move |app, r| match r {
                // A subagent's session is not one to resume.
                Ok(list) => match list.into_iter().find(|s| s.owner.is_none()) {
                    Some(info) => {
                        app.open_session(info.session_id, true);
                    }
                    None => app.info("No sessions to resume"),
                },
                Err(e) => app.error(format!("cannot list sessions: {e}")),
            }),
        }
    }

    // ---- mouse selection -----------------------------------------------------

    /// A mouse press, drag or release. Lua `mouse` handlers see it first
    /// (with the popup, or underlying chat item and layout leaf). Returning
    /// true takes it; otherwise the left button selects text and focuses
    /// panels, except under popups.
    pub fn mouse(&mut self, action: &str, button: &str, at: (u16, u16)) {
        let m = input::mouse(self, action, button, at);
        // A popup still owns this event if its handler closed it.
        if m.handled || m.popup || button != "left" {
            self.dirty = true;
            return;
        }
        match action {
            "down" => self.mouse_down(at),
            "drag" => self.mouse_drag(at),
            _ => self.mouse_up(at),
        }
    }

    /// The chat item at screen cell `at`: `{ index, line }`, or None.
    pub fn chat_at(&self, at: (u16, u16)) -> Option<serde_json::Value> {
        let area = self.placed.get(&CHAT_WIN)?.area;
        let (x, y) = at;
        if x < area.x || x >= area.right() || y < area.y || y >= area.bottom() {
            return None;
        }
        let top = self.windows.get(&CHAT_WIN)?.top;
        let row = top + (y - area.y) as usize;
        let (index, line) = self.chats[self.current].item_at_row(row)?;
        Some(serde_json::json!({ "index": index, "line": line }))
    }

    /// The chat window: scroll position and which items show.
    pub fn chat_view(&self) -> serde_json::Value {
        let area = self
            .placed
            .get(&CHAT_WIN)
            .map(|p| p.area)
            .unwrap_or_default();
        let win = self.windows.get(&CHAT_WIN);
        let top = win.map_or(0, |w| w.top);
        let height = area.height as usize;
        let rows = self.chats[self.current].item_rows();
        let visible: Vec<usize> = rows
            .iter()
            .filter(|&&(_, start, len)| len > 0 && start < top + height && start + len > top)
            .map(|&(i, _, _)| i)
            .collect();
        serde_json::json!({
            "top": top,
            "height": height,
            "rows": self.chats[self.current].row_count(),
            "follow": win.is_some_and(|w| w.follow),
            "first": visible.first(),
            "last": visible.last(),
        })
    }

    /// Scroll the chat so item `index` shows at the top, center or bottom.
    /// Returns false when there is no such drawn item.
    pub fn chat_scroll_to(&mut self, index: usize, at: &str) -> bool {
        let Some((_, start, len)) = self.chats[self.current]
            .item_rows()
            .into_iter()
            .find(|&(i, _, _)| i == index)
        else {
            return false;
        };
        let height = self
            .placed
            .get(&CHAT_WIN)
            .map_or(0, |p| p.area.height as usize);
        let top = match at {
            "bottom" => (start + len).saturating_sub(height),
            "center" => (start + len / 2).saturating_sub(height / 2),
            _ => start,
        };
        if let Some(w) = self.windows.get_mut(&CHAT_WIN) {
            w.top = top;
            w.follow = false;
        }
        self.dirty = true;
        true
    }

    /// Scroll the chat by rows, or to "top" / "bottom".
    pub fn chat_scroll(&mut self, by: Option<i64>, to: Option<&str>) {
        match (by, to) {
            (_, Some("top")) => {
                if let Some(w) = self.windows.get_mut(&CHAT_WIN) {
                    w.top = 0;
                    w.follow = false;
                }
            }
            (_, Some(_)) => {
                if let Some(w) = self.windows.get_mut(&CHAT_WIN) {
                    w.follow = true;
                }
            }
            (Some(n), None) => self.scroll(n),
            (None, None) => {}
        }
        self.dirty = true;
    }

    /// The wheel over another session's `chat:<id>` leaf scrolls that chat.
    pub fn chat_wheel(&mut self, at: (u16, u16), up: bool) -> bool {
        let pos = Position::new(at.0, at.1);
        let Some(buf) = (self.leaves.iter())
            .find(|(_, r)| r.contains(pos))
            .and_then(|(name, _)| name.strip_prefix("chat:"))
            .and_then(|id| self.chat_by_session(id))
            .filter(|&b| b != self.current)
        else {
            return false;
        };
        let w = self.chat_views.entry(buf).or_default();
        w.top = if up {
            w.top.saturating_sub(3)
        } else {
            w.top + 3
        };
        // Render clamps and re-enables follow at the bottom.
        w.follow = false;
        self.dirty = true;
        true
    }

    pub fn mouse_down(&mut self, at: (u16, u16)) {
        self.panel_click(at);
        // A selection stays inside the panel or layout leaf it starts in.
        let pos = Position::new(at.0, at.1);
        let clip = (self.panels.iter().filter_map(|p| p.area))
            .map(|a| Rect::new(a.x, a.y, a.width, a.height))
            .chain(self.leaves.iter().map(|(_, r)| *r))
            .find(|r| r.contains(pos))
            .unwrap_or(self.screen);
        self.selection = Some(crate::selection::Selection::new(at, clip));
        self.dirty = true;
    }

    pub fn mouse_drag(&mut self, at: (u16, u16)) {
        if let Some(s) = &mut self.selection
            && !s.done
        {
            s.head = at;
            self.dirty = true;
        }
    }

    /// Releasing ends the selection; the next draw copies it. A click
    /// without a drag selects nothing.
    pub fn mouse_up(&mut self, at: (u16, u16)) {
        self.mouse_drag(at);
        match &mut self.selection {
            Some(s) if !s.is_empty() => s.done = true,
            _ => self.selection = None,
        }
        self.dirty = true;
    }

    // ---- popups --------------------------------------------------------------

    /// The topmost window that takes the keyboard.
    pub fn focused_popup(&self) -> Option<&Popup> {
        bone_ui::popup::focused(&self.popups)
    }

    // ---- server events -------------------------------------------------------

    pub fn handle_server(&mut self, ev: ServerEvent) {
        if let Some(Ok(p)) = ev.parse::<AskRequested>()
            && self.asks.insert(p.ask_id, p.clone()).as_ref() == Some(&p)
        {
            return;
        }
        if let Some(Ok(p)) = ev.parse::<AskResolved>() {
            self.asks.remove(&p.ask_id);
        }
        if self.has_autocmd(&ev.method) {
            let data = ev.params.clone().unwrap_or(serde_json::Value::Null);
            self.handle_server_inner(&ev);
            self.fire(&ev.method, data);
            // Handlers change what regions show; regions draw every frame.
            self.dirty = true;
        } else {
            self.handle_server_inner(&ev);
        }
    }

    fn handle_server_inner(&mut self, ev: &ServerEvent) {
        macro_rules! on {
            ($n:ty, |$p:ident| $body:expr) => {
                if let Some(Ok($p)) = ev.parse::<$n>() {
                    $body;
                    self.dirty = true;
                    return;
                }
            };
        }
        let chats = &mut self.chats;
        if bone_ui::chat::apply_event(ev, |id| {
            let buf = bone_ui::chat::find_chat(chats, id)?;
            chats.get_mut(buf)
        }) {
            let deferred = if let Some(Ok(p)) = ev.parse::<MessageDelta>() {
                self.chats.get(self.current).is_some_and(|chat| {
                    chat.session_id() == Some(p.session_id.as_str())
                        && chat.streaming_redraw_deferred(Instant::now())
                })
            } else {
                false
            };
            self.dirty |= !deferred;
            return;
        }
        on!(SettingsChanged, |p| self.update_settings(p.settings));
        on!(ProcessChanged, |p| bone_ui::lua::processes::changed(
            self, &p
        ));
        on!(SessionUpdated, |p| self.reload_chat(p.session_id));
        on!(SessionDeleted, |p| self.session_deleted(&p.session_id));
    }

    /// Forget the tray's shells (a chat with no session, or a deleted one).
    fn clear_processes(&mut self) {
        bone_ui::lua::processes::clear(self);
    }

    pub fn request_processes(&mut self, session_id: String) {
        bone_ui::lua::processes::request(self, session_id);
    }

    pub fn process_list(&self) -> Vec<ProcessSnapshot> {
        self.processes.sorted()
    }

    /// A session was deleted: its chat becomes a fresh one (chats keep their
    /// places, which pending replies rely on).
    fn session_deleted(&mut self, session_id: &str) {
        if self.processes.session.as_deref() == Some(session_id) {
            self.clear_processes();
        }
        let Some(buf) = self.chat_by_session(session_id) else {
            return;
        };
        self.chats[buf] = ChatBuffer::new(None);
        if buf == self.current {
            self.show_chat(buf);
            self.info("session deleted");
        }
    }

    /// Core Lua changed a session's transcript: load it again if a chat
    /// shows it. No session (events were lost): load every chat again.
    fn reload_chat(&mut self, session_id: String) {
        if session_id.is_empty() {
            let ids: Vec<_> = self
                .chats
                .iter()
                .filter_map(|c| c.session_id())
                .map(str::to_owned)
                .collect();
            return ids.into_iter().for_each(|id| self.reload_chat(id));
        }
        let Some(buf) = self.chat_by_session(&session_id) else {
            return;
        };
        self.request::<SessionMessages>(
            SessionMessagesParams {
                session_id,
                tail: None,
            },
            move |app, r| match r {
                Ok(r) => {
                    if let Some(c) = app.chat_mut(buf) {
                        c.load(r.info, &r.messages, r.active_turn);
                        c.set_tool_times(&r.tool_times);
                        c.set_queue(r.queue, r.queue_paused);
                    }
                }
                Err(e) => app.error(format!("cannot reload session: {e}")),
            },
        );
    }

    /// Something is moving on screen (a spinner or elapsed time).
    pub fn animating(&self) -> bool {
        self.chats.iter().any(|c| c.turn.is_some() || c.starting) || self.processes.any_running()
    }

    pub fn server_closed(&mut self) {
        self.quit = Some(Some("the bone server closed the connection".into()));
    }

    /// The server went away and `run` is reconnecting (`--connect`). Requests
    /// fail until then; a prompt sent meanwhile comes back to its draft.
    pub fn server_lost(&mut self) {
        self.error("lost the bone server; reconnecting…");
        self.fire("server/disconnected", serde_json::json!({}));
    }

    /// Refresh snapshots before consuming the new connection's buffered events.
    pub fn reconnected(&mut self, client: Client, settings: Value, asks: Vec<AskRequestedParams>) {
        self.client = Arc::new(client);
        self.info("reconnected to the bone server");
        self.update_settings(settings);
        self.reload_chat(String::new());
        self.clear_processes();
        if let Some(id) = self.current_session_id() {
            self.request_processes(id);
        }
        let gone: Vec<_> = self
            .asks
            .values()
            .filter(|a| !asks.contains(a))
            .map(|a| a.ask_id)
            .collect();
        for ask_id in gone {
            self.handle_server(ServerEvent {
                method: "ask/resolved".into(),
                params: Some(serde_json::json!({ "ask_id": ask_id, "answer": null })),
            });
        }
        for ask in asks {
            self.handle_server(ServerEvent {
                method: "ask/requested".into(),
                params: Some(serde_json::json!(ask)),
            });
        }
        self.fire("server/reconnected", serde_json::json!({}));
    }
}

/// Hand freed memory back to the OS. Loading a long transcript goes through
/// large temporary JSON values, and glibc otherwise keeps the freed pages.
fn trim_heap() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: malloc_trim only releases free heap pages; it has no
    // preconditions.
    unsafe {
        libc::malloc_trim(0);
    }
}

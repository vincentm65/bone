//! Editor state and everything that changes it: keys, actions, server events
//! and request replies. Drawing lives in `render`.
//!
//! One view: the current chat above, the prompt below. You always type into
//! the prompt; `/` starts a command. Popups (from Lua, or the session picker)
//! take the keyboard while they are open.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bone_client::{Client, ClientError, Event as ServerEvent};
use bone_proto::Method;
use bone_proto::methods::*;
use mlua::Lua;
use ratatui::layout::Rect;
use tokio::sync::mpsc;

use crate::chat::ChatBuffer;
use crate::editor::TextBuffer;
use crate::keymap::{Action, Builtin, Context, Keymaps};
use crate::keys::Key;
use crate::layout::{BufferId, Placed, Window, WindowId};
use crate::lua::{Autocmd, UserCommand};
use crate::options::Options;
use crate::theme::Theme;

/// A second ctrl+c within this long quits.
const QUIT_WINDOW: Duration = Duration::from_secs(2);
const MAX_SUGGESTIONS: usize = 8;

pub const CHAT_WIN: WindowId = 0;
pub const PROMPT_WIN: WindowId = 1;

/// A window opened from Lua (`bone.ui.win`, `bone.ui.popup`). Lua draws
/// everything inside it; Rust only places it and clears behind it. The
/// topmost focused one has the keyboard: its own keys first, then the
/// `popup` keymaps.
#[derive(Clone)]
pub struct Popup {
    pub id: u64,
    /// Takes the keyboard while open.
    pub focus: bool,
    /// What `row`/`col` are relative to: "screen", "chat" or "prompt" (the
    /// space above the prompt, so row -1 sits right on it).
    pub anchor: String,
    /// Higher is drawn on top; ties go to the newer one.
    pub z: i32,
    /// Callback id of the lines: a function(ctx) or a fixed list.
    pub lines: u64,
    pub keys: Vec<(Key, u64)>,
    /// Called with the key name for keys `keys` does not name.
    pub on_key: Option<u64>,
    /// Size; `None` fits the lines.
    pub width: Option<u16>,
    pub height: Option<u16>,
    /// Position; `None` centers, negative counts from the bottom/right.
    pub row: Option<i32>,
    pub col: Option<i32>,
    pub opened: Instant,
    /// Keys within this long after opening are ignored.
    pub guard: Duration,
}

#[derive(Debug, Clone)]
struct RawInterceptor {
    id: u64,
    callback: u64,
    context: Option<Context>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Error,
}

/// Work to apply on the UI thread, e.g. a request reply.
pub struct AppEvent(pub(crate) Box<dyn FnOnce(&mut App) + Send>);

pub struct App {
    client: Arc<Client>,
    tx: mpsc::UnboundedSender<AppEvent>,
    pub cwd: String,
    pub chats: Vec<ChatBuffer>,
    /// The chat on screen.
    pub current: BufferId,
    pub prompt: TextBuffer,
    pub windows: HashMap<WindowId, Window>,
    /// Where the chat and prompt were drawn last frame.
    pub placed: HashMap<WindowId, Placed>,
    pub screen: Rect,
    pub keymaps: Keymaps,
    /// The explicitly focused named context (popup focus still takes precedence).
    focused_context: Option<Context>,
    pending_keys: Vec<Key>,
    pending_action: Option<Action>,
    pending_context: Option<Context>,
    pending_since: Option<Instant>,
    raw_interceptors: Vec<RawInterceptor>,
    /// Selected row of the slash-command suggestions.
    pub suggestion: usize,
    /// Suggestions are hidden (Esc) until the prompt text changes.
    suggestions_hidden_for: Option<String>,
    pub message: Option<(String, Level)>,
    /// Every message shown, for `/messages`.
    pub log: Vec<String>,
    pub options: Options,
    pub opts_rev: u64,
    /// Bumped when Lua changes views; part of every render cache key.
    pub views_rev: u64,
    pub theme: Theme,
    pub colors_name: Option<String>,
    /// `bone.ui` functions that errored; skipped until restart.
    pub ui_broken: HashSet<String>,
    pub popups: Vec<Popup>,
    /// Text highlighted with the mouse.
    pub selection: Option<crate::selection::Selection>,
    /// Text to put on the system clipboard after the next draw.
    pub clipboard: Option<String>,
    prompt_history: Vec<String>,
    prompt_history_pos: Option<usize>,
    quit_armed: Option<Instant>,
    pub dirty: bool,
    pub quit: Option<Option<String>>,
    pub lua: Option<Lua>,
    pub config_dir: Option<PathBuf>,
    pub next_callback: u64,
    pub user_commands: HashMap<String, UserCommand>,
    pub autocmds: Vec<Autocmd>,
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
            tx,
            cwd,
            chats: vec![ChatBuffer::new(None)],
            current: 0,
            prompt: TextBuffer::default(),
            windows: HashMap::from([(CHAT_WIN, window.clone()), (PROMPT_WIN, window)]),
            placed: HashMap::new(),
            screen: Rect::default(),
            keymaps: Keymaps::default(),
            focused_context: None,
            pending_keys: Vec::new(),
            pending_action: None,
            pending_context: None,
            pending_since: None,
            raw_interceptors: Vec::new(),
            suggestion: 0,
            suggestions_hidden_for: None,
            message: None,
            log: Vec::new(),
            options: Options::default(),
            opts_rev: 0,
            views_rev: 0,
            theme: Theme::default(),
            colors_name: None,
            ui_broken: HashSet::new(),
            popups: Vec::new(),
            selection: None,
            clipboard: None,
            prompt_history: Vec::new(),
            prompt_history_pos: None,
            quit_armed: None,
            dirty: true,
            quit: None,
            lua: None,
            config_dir: config_dir.clone(),
            next_callback: 0,
            user_commands: HashMap::new(),
            autocmds: Vec::new(),
        };
        crate::lua::init(&mut app, config_dir);
        app.dirty = true;
        app
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

    pub fn chat(&self, id: BufferId) -> Option<&ChatBuffer> {
        self.chats.get(id)
    }

    pub fn chat_mut(&mut self, id: BufferId) -> Option<&mut ChatBuffer> {
        self.chats.get_mut(id)
    }

    /// The chat on screen, which prompts are sent to.
    pub fn target_chat(&self) -> Option<BufferId> {
        Some(self.current)
    }

    fn chat_by_session(&self, session_id: &str) -> Option<BufferId> {
        self.chats
            .iter()
            .position(|c| c.session_id() == Some(session_id))
    }

    pub fn show_chat(&mut self, id: BufferId) {
        self.current = id;
        if let Some(w) = self.windows.get_mut(&CHAT_WIN) {
            w.top = 0;
            w.follow = true;
        }
        self.dirty = true;
    }

    /// A fresh session (created on the first message).
    pub fn new_session(&mut self) {
        let c = &self.chats[self.current];
        if c.session.is_none() && c.entries.is_empty() {
            return;
        }
        self.chats.push(ChatBuffer::new(None));
        self.show_chat(self.chats.len() - 1);
    }

    pub fn prompt_text(&self) -> String {
        self.prompt.text()
    }

    pub fn set_prompt_text(&mut self, text: &str) {
        self.prompt.set_text(text);
        self.dirty = true;
    }

    // ---- keys ----------------------------------------------------------------

    /// Which keymaps apply right now. A focused popup always wins over a
    /// named context selected by Lua.
    pub fn context(&self) -> Context {
        if self.focused_popup().is_some() {
            Context::Popup
        } else {
            self.focused_context.clone().unwrap_or(Context::Main)
        }
    }

    pub fn focus_context(&mut self, ctx: Context) -> Result<(), String> {
        match &ctx {
            Context::Main => return self.clear_context(),
            Context::Popup => return Err("popup context is controlled by focused windows".into()),
            Context::Named(_) if !self.keymaps.has_context(&ctx) => {
                return Err(format!("unknown context {}", ctx.name()));
            }
            Context::Named(_) => {}
        }
        self.flush_pending();
        self.focused_context = Some(ctx);
        self.dirty = true;
        Ok(())
    }

    pub fn clear_context(&mut self) -> Result<(), String> {
        self.flush_pending();
        self.focused_context = None;
        self.dirty = true;
        Ok(())
    }

    pub fn delete_context(&mut self, ctx: &Context) -> Result<(), String> {
        if !self.keymaps.has_context(ctx) {
            return Err(format!("unknown context {}", ctx.name()));
        }
        if self.focused_context.as_ref() == Some(ctx) {
            self.flush_pending();
            self.focused_context = None;
        }
        self.keymaps.remove_context(ctx)?;
        self.dirty = true;
        Ok(())
    }

    pub fn key_sequence_deadline(&self) -> Option<Instant> {
        self.pending_since
            .map(|since| since + Duration::from_millis(self.options.timeoutlen))
    }

    /// Expire a pending key sequence when its current `timeoutlen` deadline
    /// has passed. This is called by the terminal loop even when nothing is
    /// being redrawn.
    pub fn expire_key_sequence(&mut self) -> bool {
        if self
            .key_sequence_deadline()
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.flush_pending();
            self.dirty = true;
            true
        } else {
            false
        }
    }

    pub fn handle_key(&mut self, key: Key) {
        self.dirty = true;
        self.selection = None;
        let ctx = self.context();
        if self
            .pending_context
            .as_ref()
            .is_some_and(|pending| pending != &ctx)
        {
            self.flush_pending();
        }
        self.dispatch_key(key, true);
    }

    fn dispatch_key(&mut self, key: Key, allow_raw: bool) {
        let ctx = self.context();
        if ctx == Context::Popup {
            return self.popup_key(key, allow_raw);
        }
        if self.message.is_some() {
            self.message = None;
        }
        if allow_raw && self.raw_intercepts(&ctx, key) {
            self.discard_pending();
            return;
        }
        self.keymap_key(ctx, key);
    }

    fn keymap_key(&mut self, ctx: Context, key: Key) {
        let had_pending = !self.pending_keys.is_empty();
        let mut sequence = self.pending_keys.clone();
        sequence.push(key);
        let lookup = self.keymaps.lookup(&ctx, &sequence);

        if lookup.prefix {
            self.set_pending(sequence, lookup.action, ctx);
            return;
        }

        if let Some(action) = lookup.action {
            self.discard_pending();
            if !self.dispatch_action(action) {
                self.replay_literal(&ctx, &sequence);
            }
            return;
        }

        if !had_pending {
            self.unmapped_in_context(&ctx, key);
            return;
        }

        let (pending, fallback, pending_ctx) = self.take_pending();
        if let Some(action) = fallback {
            if !self.dispatch_action(action) && self.context() == pending_ctx {
                self.replay_literal(&pending_ctx, &pending);
            }
        } else if self.context() == pending_ctx {
            // A prefix with no exact fallback is literal when the next key
            // proves it was not a sequence.
            self.replay_literal(&pending_ctx, &pending);
        }
        // The mismatching key was not consumed by the failed sequence.
        self.dispatch_key(key, false);
    }

    fn replay_literal(&mut self, ctx: &Context, keys: &[Key]) {
        for &key in keys {
            self.unmapped_in_context(ctx, key);
        }
    }

    fn unmapped_in_context(&mut self, ctx: &Context, key: Key) {
        let Some(c) = key.text() else { return };
        match ctx {
            Context::Main | Context::Named(_) => {
                self.prompt_history_pos = None;
                self.prompt.insert_char(c);
            }
            Context::Popup => {}
        }
    }

    pub fn paste(&mut self, text: &str) {
        self.dirty = true;
        self.flush_pending();
        match self.context() {
            Context::Main | Context::Named(_) => self.prompt.insert_str(text),
            Context::Popup => {}
        }
    }

    pub fn run(&mut self, action: Action) {
        self.dirty = true;
        let _ = self.dispatch_action(action);
    }

    fn dispatch_action(&mut self, action: Action) -> bool {
        self.dirty = true;
        match action {
            Action::Builtin(b) => {
                self.builtin(b);
                true
            }
            Action::Command(cmd) => {
                self.execute(&cmd);
                true
            }
            Action::Lua(id) => !matches!(
                self.call_callback(id, "keymap", |_| Ok(mlua::Value::Nil)),
                Some(mlua::Value::Boolean(false))
            ),
        }
    }

    fn set_pending(&mut self, keys: Vec<Key>, action: Option<Action>, ctx: Context) {
        self.pending_keys = keys;
        self.pending_action = action;
        self.pending_context = Some(ctx);
        self.pending_since = Some(Instant::now());
    }

    fn take_pending(&mut self) -> (Vec<Key>, Option<Action>, Context) {
        let keys = std::mem::take(&mut self.pending_keys);
        let action = self.pending_action.take();
        let ctx = self.pending_context.take().unwrap_or(Context::Main);
        self.pending_since = None;
        (keys, action, ctx)
    }

    fn discard_pending(&mut self) {
        self.pending_keys.clear();
        self.pending_action = None;
        self.pending_context = None;
        self.pending_since = None;
    }

    fn flush_pending(&mut self) {
        if self.pending_keys.is_empty() {
            self.discard_pending();
            return;
        }
        let (keys, action, ctx) = self.take_pending();
        if let Some(action) = action
            && !self.dispatch_action(action)
            && self.context() == ctx
        {
            self.replay_literal(&ctx, &keys);
        }
    }

    pub fn add_raw_interceptor(&mut self, callback: u64, context: Option<Context>) -> u64 {
        self.next_callback += 1;
        let id = self.next_callback;
        self.raw_interceptors.push(RawInterceptor {
            id,
            callback,
            context,
        });
        id
    }

    pub fn remove_raw_interceptor(&mut self, id: u64) -> Option<u64> {
        let i = self.raw_interceptors.iter().position(|raw| raw.id == id)?;
        Some(self.raw_interceptors.remove(i).callback)
    }

    fn raw_intercepts(&mut self, ctx: &Context, key: Key) -> bool {
        let name = crate::keys::format(&key);
        let callbacks: Vec<u64> = self
            .raw_interceptors
            .iter()
            .filter(|raw| raw.context.as_ref().is_none_or(|wanted| wanted == ctx))
            .map(|raw| raw.callback)
            .collect();
        callbacks.into_iter().any(|callback| {
            matches!(
                self.call_callback(callback, "raw key", |lua| {
                    Ok(mlua::Value::String(lua.create_string(&name)?))
                }),
                Some(mlua::Value::Boolean(true))
            )
        })
    }

    fn edit(&mut self, f: impl FnOnce(&mut TextBuffer)) {
        f(&mut self.prompt);
    }

    fn builtin(&mut self, b: Builtin) {
        use Builtin::*;
        if b != Interrupt {
            self.quit_armed = None;
        }
        match b {
            Submit => self.submit_or_command(),
            Newline => self.prompt.newline(),
            Left => self.edit(|t| t.left()),
            Right => self.edit(|t| t.right()),
            Up | Down => self.vertical(b == Up),
            WordLeft => self.edit(|t| t.word_back()),
            WordRight => self.edit(|t| t.word_forward()),
            LineStart => self.edit(|t| t.line_start()),
            LineEnd => self.edit(|t| t.line_end()),
            Backspace => self.edit(|t| t.backspace()),
            Delete => self.edit(|t| t.delete()),
            DeleteWord => self.edit(|t| t.delete_word_back()),
            DeleteToStart => self.edit(|t| t.delete_to_line_start()),
            DeleteToEnd => self.edit(|t| t.delete_to_line_end()),
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
            Complete => {
                if let Some((name, _)) = self.suggestions().get(self.suggestion) {
                    let text = format!("/{name} ");
                    self.prompt.set_text(&text);
                }
            }
            Dismiss => {
                if !self.suggestions().is_empty() {
                    self.suggestions_hidden_for = Some(self.prompt.text());
                } else {
                    self.message = None;
                }
            }
            Interrupt => self.interrupt(),
            Quit => self.quit = Some(None),
            QuitIfEmpty => {
                if self.prompt.is_empty() {
                    self.quit = Some(None);
                } else {
                    self.prompt.delete();
                }
            }
            NewSession => self.new_session(),
            Sessions => self.open_picker(),
        }
    }

    /// Up/Down: move through suggestions when they are showing; else move
    /// between prompt lines, and past the first/last line through history.
    fn vertical(&mut self, up: bool) {
        let n = self.suggestions().len();
        if n > 0 {
            self.suggestion = if up {
                (self.suggestion + n - 1) % n
            } else {
                (self.suggestion + 1) % n
            };
            return;
        }
        let moved = if up {
            self.prompt.up()
        } else {
            self.prompt.down()
        };
        if moved || self.prompt_history.is_empty() {
            return;
        }
        let len = self.prompt_history.len();
        let pos = match (self.prompt_history_pos, up) {
            (None, true) => Some(len - 1),
            (None, false) => return,
            (Some(p), true) => Some(p.saturating_sub(1)),
            (Some(p), false) if p + 1 < len => Some(p + 1),
            (Some(_), false) => None,
        };
        self.prompt_history_pos = pos;
        let text = pos
            .map(|p| self.prompt_history[p].clone())
            .unwrap_or_default();
        self.prompt.set_text(&text);
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
    fn interrupt(&mut self) {
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
        if !self.prompt.is_empty() {
            self.prompt.clear();
            return;
        }
        if self.quit_armed.is_some_and(|t| t.elapsed() < QUIT_WINDOW) {
            self.quit = Some(None);
            return;
        }
        self.quit_armed = Some(Instant::now());
        self.info("Press ctrl+c again to quit");
    }

    // ---- slash commands ------------------------------------------------------

    /// Matching commands while the prompt holds a partial `/name`.
    pub fn suggestions(&self) -> Vec<(String, String)> {
        if self.focused_popup().is_some() {
            return Vec::new();
        }
        let text = self.prompt.text();
        let Some(word) = text.strip_prefix('/') else {
            return Vec::new();
        };
        if word.starts_with('/')
            || word.contains(char::is_whitespace)
            || self.suggestions_hidden_for.as_ref() == Some(&text)
        {
            return Vec::new();
        }
        let mut out: Vec<(String, String)> = crate::commands::COMMANDS
            .iter()
            .flat_map(|c| {
                std::iter::once(c.name)
                    .chain(c.aliases.iter().copied())
                    .map(move |n| (n, c))
            })
            .filter(|(n, _)| n.starts_with(word))
            .map(|(n, c)| (n.to_owned(), c.help.to_owned()))
            .chain(
                self.user_commands
                    .iter()
                    .filter(|(n, _)| n.starts_with(word))
                    .map(|(n, c)| (n.clone(), c.desc.clone())),
            )
            .collect();
        out.sort();
        out.dedup_by(|a, b| a.0 == b.0);
        out.truncate(MAX_SUGGESTIONS);
        out
    }

    /// Enter: run a `/command`, or send the prompt as a message. `//text`
    /// sends `/text`.
    fn submit_or_command(&mut self) {
        let text = self.prompt.text();
        let trimmed = text.trim_start();
        let Some(rest) = trimmed.strip_prefix('/') else {
            return self.submit();
        };
        if rest.starts_with('/') {
            self.prompt.set_text(&text.replacen("//", "/", 1));
            return self.submit();
        }
        let word = rest.split_whitespace().next().unwrap_or("");
        // Paths like /etc/hosts are messages, not commands.
        if word.contains('/') {
            return self.submit();
        }
        let mut line = rest.to_owned();
        if !self.is_command(word) {
            match self.suggestions().get(self.suggestion) {
                Some((name, _)) => line = format!("{name}{}", &rest[word.len()..]),
                None => {
                    return self.error(format!(
                        "Unknown command /{word} (start with // to send it as a message)"
                    ));
                }
            }
        }
        if self.prompt_history.last() != Some(&text) {
            self.prompt_history.push(text);
        }
        self.prompt_history_pos = None;
        self.prompt.clear();
        self.suggestion = 0;
        self.execute(&line);
    }

    fn is_command(&self, word: &str) -> bool {
        crate::commands::resolve(word).is_some() || self.user_commands.contains_key(word)
    }

    // ---- turns ---------------------------------------------------------------

    fn submit(&mut self) {
        let mut text = self.prompt.text();
        if text.trim().is_empty() {
            return;
        }
        // `submit` autocommands may cancel (false) or rewrite (a string).
        for v in self.fire("submit", serde_json::json!({ "text": text })) {
            match v {
                mlua::Value::Boolean(false) => {
                    self.prompt.clear();
                    return;
                }
                mlua::Value::String(s) => text = s.to_string_lossy(),
                _ => {}
            }
        }
        if text.trim().is_empty() {
            return;
        }
        let buf = self.current;
        let chat = &mut self.chats[buf];
        if chat.turn.is_some() || chat.starting {
            return self.error("A turn is still running (ctrl+c cancels it)");
        }
        chat.starting = true;
        self.prompt.clear();
        self.prompt_history_pos = None;
        if self.prompt_history.last() != Some(&text) {
            self.prompt_history.push(text.clone());
        }
        match self.chats[buf].session_id() {
            Some(id) => self.start_turn(buf, id.to_owned(), text),
            None => {
                let cwd = Some(self.cwd.clone());
                self.request::<SessionCreate>(SessionCreateParams { cwd }, move |app, r| match r {
                    Ok(info) => {
                        let id = info.session_id.clone();
                        if let Some(c) = app.chat_mut(buf) {
                            c.session = Some(info);
                        }
                        app.start_turn(buf, id, text);
                    }
                    Err(e) => {
                        app.turn_not_started(buf, text, format!("cannot create session: {e}"))
                    }
                });
            }
        }
    }

    fn start_turn(&mut self, buf: BufferId, session_id: String, text: String) {
        let params = TurnStartParams {
            session_id,
            text: text.clone(),
        };
        self.request::<TurnStart>(params, move |app, r| {
            if let Some(c) = app.chat_mut(buf) {
                c.starting = false;
            }
            if let Err(e) = r {
                app.turn_not_started(buf, text, format!("cannot start turn: {e}"));
            }
        });
    }

    fn turn_not_started(&mut self, buf: BufferId, text: String, why: String) {
        if let Some(c) = self.chat_mut(buf) {
            c.starting = false;
        }
        // Give the text back rather than losing it.
        if self.prompt.is_empty() {
            self.prompt.set_text(&text);
        }
        self.error(why);
    }

    /// Show a session, loading it if no chat has it yet.
    pub fn open_session(&mut self, session_id: String) {
        if let Some(buf) = self.chat_by_session(&session_id) {
            return self.show_chat(buf);
        }
        // Reuse an untouched new chat rather than piling them up.
        let c = &self.chats[self.current];
        let buf = if c.session.is_none() && c.entries.is_empty() {
            self.current
        } else {
            self.chats.push(ChatBuffer::new(None));
            self.chats.len() - 1
        };
        self.show_chat(buf);
        self.request::<SessionMessages>(SessionRef { session_id }, move |app, r| match r {
            Ok(r) => {
                if let Some(c) = app.chat_mut(buf) {
                    c.load(r.info, &r.messages, r.active_turn);
                }
            }
            Err(e) => {
                if let Some(c) = app.chat_mut(buf) {
                    c.notice(format!("cannot load session: {e}"), true);
                }
            }
        });
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
                "on: the wheel scrolls, dragging copies (/set nomouse for the terminal's own selection)".into()
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
        if self.ui_broken.is_empty() {
            out.push(item(
                "tui lua",
                "ok",
                "no errors in views, regions or windows".into(),
            ));
        } else {
            let mut names: Vec<&String> = self.ui_broken.iter().collect();
            names.sort();
            let names: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
            out.push(item(
                "tui lua",
                "error",
                format!(
                    "switched off after errors: {} (see /messages)",
                    names.join(", ")
                ),
            ));
        }
        out
    }

    /// Resume a session at startup: `None` for the newest one.
    pub fn resume(&mut self, id: Option<String>) {
        match id {
            Some(id) => self.open_session(id),
            None => self.request::<SessionList>(Empty {}, move |app, r| match r {
                Ok(list) => match list.into_iter().next() {
                    Some(info) => app.open_session(info.session_id),
                    None => app.info("No sessions to resume"),
                },
                Err(e) => app.error(format!("cannot list sessions: {e}")),
            }),
        }
    }

    // ---- mouse selection -----------------------------------------------------

    pub fn mouse_down(&mut self, at: (u16, u16)) {
        self.selection = Some(crate::selection::Selection::new(at));
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

    /// Windows in drawing order, bottom first.
    pub fn popups_in_order(&self) -> Vec<&Popup> {
        let mut v: Vec<&Popup> = self.popups.iter().collect();
        v.sort_by_key(|p| (p.z, p.id));
        v
    }

    /// The topmost window that takes the keyboard.
    pub fn focused_popup(&self) -> Option<&Popup> {
        self.popups_in_order().into_iter().rev().find(|p| p.focus)
    }

    fn popup_key(&mut self, key: Key, allow_raw: bool) {
        let Some((guard, own, on_key)) = self.focused_popup().map(|top| {
            (
                top.opened.elapsed() < top.guard,
                top.keys
                    .iter()
                    .find(|(mapped, _)| *mapped == key)
                    .map(|(_, cb)| *cb),
                top.on_key,
            )
        }) else {
            return;
        };
        if guard {
            self.discard_pending();
            return;
        }
        if let Some(cb) = own {
            self.discard_pending();
            self.call_callback(cb, "popup key", |_| Ok(mlua::Value::Nil));
            return;
        }
        if let Some(cb) = on_key {
            let name = crate::keys::format(&key);
            let r = self.call_callback(cb, "popup on_key", |lua| {
                Ok(mlua::Value::String(lua.create_string(&name)?))
            });
            self.dirty = true;
            // Returning nil or false passes the key on to the keymaps.
            if !matches!(
                r,
                None | Some(mlua::Value::Nil | mlua::Value::Boolean(false))
            ) {
                self.discard_pending();
                return;
            }
        }
        let ctx = self.context();
        if ctx != Context::Popup {
            return self.dispatch_key(key, false);
        }
        if allow_raw && self.raw_intercepts(&ctx, key) {
            self.discard_pending();
            return;
        }
        self.keymap_key(ctx, key);
    }

    // ---- server events -------------------------------------------------------

    pub fn handle_server(&mut self, ev: ServerEvent) {
        if self.has_autocmd(&ev.method) {
            let data = ev.params.clone().unwrap_or(serde_json::Value::Null);
            self.handle_server_inner(&ev);
            self.fire(&ev.method, data);
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
        on!(TurnStarted, |p| self.with_chat(&p.session_id, |c| c
            .turn_started(p.turn_id, &p.text)));
        on!(MessageDelta, |p| self
            .with_chat(&p.session_id, |c| c.delta(&p)));
        on!(MessageCompleted, |p| self
            .with_chat(&p.session_id, |c| c.message_completed(&p)));
        on!(ToolStarted, |p| self
            .with_chat(&p.session_id, |c| c.tool_started(&p)));
        on!(ToolFinished, |p| self
            .with_chat(&p.session_id, |c| c.tool_finished(&p)));
        on!(TurnFinished, |p| self
            .with_chat(&p.session_id, |c| c.turn_finished(&p)));
    }

    fn with_chat(&mut self, session_id: &str, f: impl FnOnce(&mut ChatBuffer)) {
        if let Some(buf) = self.chat_by_session(session_id) {
            f(&mut self.chats[buf]);
        }
    }

    /// Something is moving on screen (a spinner or elapsed time).
    pub fn animating(&self) -> bool {
        self.chats.iter().any(|c| c.turn.is_some() || c.starting)
    }

    pub fn server_closed(&mut self) {
        self.quit = Some(Some("the bone server closed the connection".into()));
    }
}

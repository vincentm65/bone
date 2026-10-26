//! Desktop bone client.
//!
//! A second frontend beside the TUI, built on the same `bone-ui` state: the
//! transcript reducer, the event handling, and the bare item text are
//! shared; this crate owns the window, drawing, and input. It talks to the
//! core only through a [`bone_client::Client`], so it runs the same against
//! an in-process server or a socket, next to a TUI on the same core.
//!
//! [`Gui`] is the client state and logic; `view.rs` draws a frame of it,
//! which is Lua's node tree (`node.rs`, laid out by `flex.rs`, painted by
//! `nodeview`), `native.rs` opens the window, and [`Headless`] drives it
//! without one.

mod canvas;
mod chats;
mod flex;
mod headless;
mod images;
mod keys;
mod lua;
mod native;
mod node;
mod nodeview;
mod preview;
mod profile;
mod prompt;
mod system;
mod view;
mod window;

pub use profile::{enable as enable_profile, report as profile_report, reset as profile_reset};

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use bone_client::{Client, Event, EventStream};
use bone_proto::Connection;
use bone_proto::Method;
use bone_proto::methods::{
    AskList, AskRequested, AskRequestedParams, AskResolved, AskRespond, AskRespondParams, Empty,
    ProcessChanged, QueueAdd, QueueAddParams, QueueMode, SessionCreate, SessionCreateParams,
    SessionCreated, SessionDeleted, SessionList, SessionMessages, SessionMessagesParams,
    SessionUpdated, SettingsChanged, SettingsGet,
};
use bone_proto::types::{ImageAttachment, SessionInfo};
use bone_ui::chat::RunningTurn;
use serde_json::Value;
use tokio::runtime::Handle;
use tokio::sync::mpsc;

pub use crate::headless::Headless;
pub use crate::native::run;

/// A chat. The line cache is `()`: the GUI draws items as nodes and uses
/// the shared cache only to ask `stale_key` which of them changed.
pub type ChatBuffer = bone_ui::chat::ChatBuffer<()>;

pub struct GuiOptions {
    /// Where new sessions start.
    pub cwd: String,
    /// The socket to connect to again if the server goes away; `None` for
    /// an in-process server, which cannot.
    pub reconnect: Option<PathBuf>,
    /// Where the user's config, plugins and runtime overrides live; `None`
    /// runs the built-in UI only.
    pub config_dir: Option<PathBuf>,
}

/// Work for the UI thread: a reply, a server event, or a new connection.
enum Msg {
    Apply(Box<dyn FnOnce(&mut Gui) + Send>),
    Event(Event),
    Lost,
    Reconnected(Box<bone_client::Reconnected>),
}

/// Something to tell the user, in the status line.
#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub text: String,
    pub error: bool,
    /// When it was shown, so a view can let it fade.
    pub at: Instant,
}

impl Status {
    pub fn new(text: String, error: bool) -> Self {
        Status {
            text,
            error,
            at: Instant::now(),
        }
    }
}

pub struct Gui {
    client: Arc<Client>,
    rt: Handle,
    tx: mpsc::UnboundedSender<Msg>,
    rx: mpsc::UnboundedReceiver<Msg>,
    /// Asks the window for a frame when a message arrives.
    repaint: Arc<OnceLock<eframe::egui::Context>>,
    opts: GuiOptions,
    pub chats: Vec<ChatBuffer>,
    /// The chat on screen, and since when.
    pub current: usize,
    shown_at: Instant,
    /// Chats whose earlier turns are being loaded.
    earlier: std::collections::HashSet<usize>,
    /// The core's sessions, newest first, without owned (sub-agent) ones.
    pub sessions: Vec<SessionInfo>,
    /// Questions waiting for an answer, by id (oldest first).
    pub asks: BTreeMap<u64, AskRequestedParams>,
    pub settings: Value,
    /// The prompt's text and images, the image reads under way, and the
    /// draft's number (a new one when it is sent or cleared, so a late
    /// image does not join the next).
    pub draft: String,
    pub images: Vec<ImageAttachment>,
    image_reads: usize,
    draft_epoch: u64,
    /// The core's shell processes of the chat on screen.
    pub(crate) processes: bone_ui::lua::processes::Processes,
    /// The prompt's cursor, selection and history.
    pub(crate) prompt: crate::prompt::PromptState,
    /// `/tui use` switched libraries from this one: reload next frame.
    pub(crate) library_prev: Option<String>,
    /// `/gui use` switched the GUI's library from this one (none chosen
    /// before: `None` inside).
    pub(crate) gui_library_prev: Option<Option<String>>,
    /// The drafts of the chats not on screen.
    drafts: std::collections::HashMap<usize, (String, Vec<ImageAttachment>)>,
    pub status: Option<Status>,
    /// The server went away and it is not coming back.
    pub closed: bool,
    /// What draws the window: the renderer and adapter (`None` headless).
    pub renderer: Option<String>,
    /// The window was opened see-through (`opacity` below 1 then), so its
    /// background can take the option's alpha.
    pub transparent: bool,
    /// The window as Lua sees it (`bone.gui.window_info`), the title Lua
    /// set, and window changes to send on the next frame.
    pub window_info: Value,
    pub window_title: Option<String>,
    viewport: Vec<eframe::egui::ViewportCommand>,
    /// `opts_rev` last applied to egui's theme, so a colorscheme change
    /// restyles dialogs without rewriting the style every frame.
    visuals_rev: Option<u64>,
    /// The Lua state and what the Lua API keeps.
    pub lua: crate::lua::LuaSide,
    /// What the node view keeps between frames; it outlives Lua reloads.
    pub view: crate::nodeview::ViewState,
    /// The draft last measured, shared with the frame's font metrics so an
    /// idle frame does not copy it.
    draft_rc: Rc<str>,
    /// Hash of the Lua sources last seen, and whether a check is running.
    /// `None` until the window is first focused.
    config_hash: Option<u64>,
    config_checking: bool,
    /// The window had the focus last frame, so gaining it can be told apart.
    was_focused: bool,
    /// Images read to be shown, and the one open in a window.
    previews: crate::preview::Previews,
}

impl Gui {
    /// Handshake on `conn` and load what a client starts from: settings,
    /// open questions, and the session list. Must run inside a Tokio
    /// runtime, which requests are spawned on.
    pub async fn connect(conn: Connection, opts: GuiOptions) -> Result<Gui, String> {
        let (client, events) = Client::new(conn);
        client
            .initialize("bone-gui")
            .await
            .map_err(|e| format!("cannot connect: {e}"))?;
        let settings = client
            .request::<SettingsGet>(Empty {})
            .await
            .map_err(|e| e.to_string())?;
        let asks = client
            .request::<AskList>(Empty {})
            .await
            .map_err(|e| e.to_string())?;
        let (tx, rx) = mpsc::unbounded_channel();
        let mut gui = Gui {
            client: Arc::new(client),
            rt: Handle::current(),
            tx,
            rx,
            repaint: Arc::new(OnceLock::new()),
            opts,
            chats: vec![ChatBuffer::new(None)],
            current: 0,
            sessions: Vec::new(),
            asks: asks.into_iter().map(|a| (a.ask_id, a)).collect(),
            settings,
            draft: String::new(),
            images: Vec::new(),
            image_reads: 0,
            draft_epoch: 0,
            shown_at: Instant::now(),
            earlier: Default::default(),
            processes: Default::default(),
            prompt: Default::default(),
            library_prev: None,
            gui_library_prev: None,
            drafts: Default::default(),
            status: None,
            closed: false,
            renderer: None,
            transparent: false,
            window_info: Value::Null,
            window_title: None,
            viewport: Vec::new(),
            visuals_rev: None,
            lua: Default::default(),
            view: Default::default(),
            previews: Default::default(),
            draft_rc: Rc::from(""),
            config_hash: None,
            config_checking: false,
            was_focused: false,
        };
        gui.forward(events);
        gui.refresh_sessions();
        gui.load_lua();
        Ok(gui)
    }

    /// Forward the connection's events to the UI thread; `Lost` when it ends.
    fn forward(&self, mut events: EventStream) {
        let (tx, repaint) = (self.tx.clone(), self.repaint.clone());
        self.rt.spawn(async move {
            while let Some(ev) = events.recv().await {
                if tx.send(Msg::Event(ev)).is_err() {
                    return;
                }
                wake(&repaint);
            }
            let _ = tx.send(Msg::Lost);
            wake(&repaint);
        });
    }

    /// Send `M` and handle its reply on the UI thread.
    fn request<M: Method>(
        &self,
        params: M::Params,
        done: impl FnOnce(&mut Gui, Result<M::Result, String>) + Send + 'static,
    ) where
        M::Params: Send + 'static,
        M::Result: Send + 'static,
    {
        let (client, tx, repaint) = (self.client.clone(), self.tx.clone(), self.repaint.clone());
        self.rt.spawn(async move {
            let r = client.request::<M>(params).await.map_err(|e| e.to_string());
            let _ = tx.send(Msg::Apply(Box::new(move |gui| done(gui, r))));
            wake(&repaint);
        });
    }

    /// Handle every message that has arrived; true if anything changed.
    pub fn pump(&mut self) -> bool {
        let mut changed = false;
        while let Ok(msg) = self.rx.try_recv() {
            self.handle(msg);
            changed = true;
        }
        changed
    }

    /// Wait for the next message and handle it; false once none can come.
    pub async fn next(&mut self) -> bool {
        match self.rx.recv().await {
            Some(msg) => {
                self.handle(msg);
                true
            }
            None => false,
        }
    }

    fn handle(&mut self, msg: Msg) {
        self.view.invalidate();
        match msg {
            Msg::Apply(f) => f(self),
            Msg::Event(ev) => self.handle_event(&ev),
            Msg::Lost => self.lost(),
            Msg::Reconnected(r) => self.reconnected(*r),
        }
    }

    /// A server event, as the TUI handles it: transcript events through the
    /// shared reducer, the rest here; then Lua's handlers of it.
    pub fn handle_event(&mut self, ev: &Event) {
        self.apply_event(ev);
        let data = ev.params.clone().unwrap_or(Value::Null);
        self.fire_event(&ev.method, data);
    }

    fn apply_event(&mut self, ev: &Event) {
        let chats = &mut self.chats;
        if bone_ui::chat::apply_event(ev, |id| {
            chats.iter_mut().find(|c| c.session_id() == Some(id))
        }) {
            return;
        }
        if let Some(Ok(p)) = ev.parse::<ProcessChanged>() {
            bone_ui::lua::processes::changed(self, &p);
        } else if let Some(Ok(p)) = ev.parse::<AskRequested>() {
            self.asks.insert(p.ask_id, p);
        } else if let Some(Ok(p)) = ev.parse::<AskResolved>() {
            self.asks.remove(&p.ask_id);
        } else if let Some(Ok(p)) = ev.parse::<SettingsChanged>() {
            let previous = std::mem::replace(&mut self.settings, p.settings);
            self.apply_gui_settings(Some(&previous["gui"]));
        } else if let Some(Ok(p)) = ev.parse::<SessionUpdated>() {
            // `lagged` (no session): events were missed, so reload them all.
            if p.session_id.is_empty() {
                self.reload_all();
            } else {
                self.reload(&p.session_id);
            }
            self.refresh_sessions();
        } else if let Some(Ok(p)) = ev.parse::<SessionDeleted>() {
            self.session_deleted(&p.session_id);
        } else if ev.parse::<SessionCreated>().is_some() {
            self.refresh_sessions();
        }
    }

    fn refresh_sessions(&self) {
        self.request::<SessionList>(Empty {}, |gui, r| match r {
            Ok(list) => gui.sessions = list.into_iter().filter(|s| s.owner.is_none()).collect(),
            Err(e) => gui.error(format!("cannot list sessions: {e}")),
        });
    }

    fn chat_of(&self, session_id: &str) -> Option<usize> {
        self.chats
            .iter()
            .position(|c| c.session_id() == Some(session_id))
    }

    /// A new, unsaved chat; the session is created by its first message.
    pub fn new_chat(&mut self) {
        let c = &self.chats[self.current];
        if c.session.is_some() || !c.entries.is_empty() {
            self.chats.push(ChatBuffer::new(None));
            self.show_chat(self.chats.len() - 1);
        }
    }

    /// Load chat `buf` again: as much of it as it had.
    fn load(&self, buf: usize, session_id: String) {
        let partial = self.chats.get(buf).is_some_and(|c| c.omitted_turns > 0);
        let tail = partial.then_some(chats::TAIL);
        self.request::<SessionMessages>(
            SessionMessagesParams { session_id, tail },
            move |gui, r| match r {
                Ok(r) => gui.apply_messages(buf, r),
                Err(e) => {
                    if let Some(c) = gui.chats.get_mut(buf) {
                        c.notice(format!("cannot load session: {e}"), true);
                    }
                }
            },
        );
    }

    fn reload(&self, session_id: &str) {
        if let Some(buf) = self.chat_of(session_id) {
            self.load(buf, session_id.to_owned());
        }
    }

    fn reload_all(&self) {
        for (buf, c) in self.chats.iter().enumerate() {
            if let Some(id) = c.session_id() {
                self.load(buf, id.to_owned());
            }
        }
    }

    /// A session was deleted: its chat becomes a fresh one (chats keep
    /// their places, which pending replies rely on).
    fn session_deleted(&mut self, id: &str) {
        if self.processes.session.as_deref() == Some(id) {
            bone_ui::lua::processes::clear(self);
        }
        if let Some(buf) = self.chat_of(id) {
            self.chats[buf] = ChatBuffer::new(None);
            if buf == self.current {
                self.show_chat(buf);
                self.info("session deleted".into());
            }
        }
        self.refresh_sessions();
    }

    /// Send the prompt to the chat on screen (see [`Gui::submit_as`]).
    pub fn submit(&mut self) {
        self.submit_as(None);
    }

    /// Send the prompt to the chat on screen: it starts a turn on an idle
    /// session and queues otherwise, so it never conflicts with another
    /// client. `mode` is how a queued message joins a running turn (default:
    /// the `queue_mode` option). A new chat's session is created first.
    pub fn submit_as(&mut self, mode: Option<QueueMode>) {
        let text = self.expanded().trim().to_owned();
        if text.is_empty() && self.images.is_empty() {
            return;
        }
        if self.image_reads > 0 {
            return self.error("an image is still being read".into());
        }
        let images = self.images.clone();
        let Some(text) = bone_ui::lua::fire_submit(self, &text, &images) else {
            self.clear_draft();
            self.prompt_changed();
            return;
        };
        if text.trim().is_empty() && self.images.is_empty() {
            return;
        }
        let mode = mode.unwrap_or_else(|| bone_ui::options::queue_mode(&self.lua.dynamic_options));
        let images = std::mem::take(&mut self.images);
        self.remember(&text, &images);
        self.clear_draft();
        let buf = self.current;
        self.chats[buf].starting = true;
        match self.chats[buf].session_id().map(str::to_owned) {
            Some(session_id) => self.queue(buf, session_id, text, images, mode),
            None => {
                let cwd = Some(self.opts.cwd.clone());
                self.request::<SessionCreate>(SessionCreateParams { cwd }, move |gui, r| match r {
                    Ok(info) => {
                        let session_id = info.session_id.clone();
                        if let Some(c) = gui.chats.get_mut(buf) {
                            c.session = Some(info);
                        }
                        if buf == gui.current {
                            bone_ui::lua::processes::request(gui, session_id.clone());
                        }
                        gui.queue(buf, session_id, text, images, mode);
                    }
                    Err(e) => gui.unsent(buf, text, images, e),
                });
            }
        }
    }

    fn queue(
        &self,
        buf: usize,
        session_id: String,
        text: String,
        images: Vec<ImageAttachment>,
        mode: QueueMode,
    ) {
        let params = QueueAddParams {
            session_id,
            text: text.clone(),
            mode,
            images: images.clone(),
        };
        self.request::<QueueAdd>(params, move |gui, r| match r {
            Ok(r) => {
                // Started (`turn_id`) or queued; running once accepted, unless
                // it already finished.
                if let Some(c) = gui.chats.get_mut(buf).filter(|c| c.starting) {
                    c.starting = false;
                    if let Some(turn_id) = r.turn_id {
                        c.turn.get_or_insert(RunningTurn {
                            turn_id,
                            started: Instant::now(),
                        });
                    }
                }
            }
            Err(e) => gui.unsent(buf, text, images, e),
        });
    }

    /// Empty the prompt, text and images.
    pub(crate) fn clear_draft(&mut self) {
        self.draft.clear();
        self.images.clear();
        self.draft_epoch += 1;
        self.prompt.pastes.clear();
        self.cursor_to_end();
    }

    /// A prompt that did not go out comes back to the draft.
    fn unsent(&mut self, buf: usize, text: String, images: Vec<ImageAttachment>, e: String) {
        if let Some(c) = self.chats.get_mut(buf) {
            c.starting = false;
        }
        if self.draft.is_empty() {
            self.draft = text;
        }
        self.images.splice(0..0, images);
        self.error(format!("not sent: {e}"));
    }

    /// Answer question `ask_id`. Another client may have answered first;
    /// `ask/resolved` closes it either way.
    pub fn answer(&self, ask_id: u64, answer: Value) {
        self.request::<AskRespond>(AskRespondParams { ask_id, answer }, |gui, r| {
            if let Err(e) = r {
                gui.error(format!("cannot answer: {e}"));
            }
        });
    }

    fn lost(&mut self) {
        let Some(path) = self.opts.reconnect.clone() else {
            self.closed = true;
            return self.error("the bone server closed the connection".into());
        };
        self.error("lost the bone server; reconnecting…".into());
        self.fire_event("server/disconnected", serde_json::json!({}));
        let (tx, repaint) = (self.tx.clone(), self.repaint.clone());
        self.rt.spawn(async move {
            let r = bone_client::reconnect(path, "bone-gui").await;
            let _ = tx.send(Msg::Reconnected(Box::new(r)));
            wake(&repaint);
        });
    }

    /// A new connection: take its snapshots before its buffered events.
    fn reconnected(&mut self, r: bone_client::Reconnected) {
        self.client = Arc::new(r.client);
        self.settings = r.settings;
        self.asks = r.asks.into_iter().map(|a| (a.ask_id, a)).collect();
        self.reload_all();
        self.refresh_sessions();
        bone_ui::lua::processes::clear(self);
        let current = self.chats[self.current].session_id().map(str::to_owned);
        bone_ui::lua::processes::follow(self, current);
        self.forward(r.events);
        self.info("reconnected to the bone server".into());
        self.fire_event("server/reconnected", serde_json::json!({}));
    }

    pub fn info(&mut self, text: String) {
        self.status = Some(Status::new(text, false));
    }

    pub fn error(&mut self, text: String) {
        self.status = Some(Status::new(text, true));
    }

    /// The chat items in view, as text, a line to each piece of it (after a
    /// frame).
    pub fn transcript(&self) -> String {
        self.view.transcript()
    }

    /// The current chat's title, for the window.
    pub fn title(&self) -> String {
        self.chats[self.current].title()
    }

    pub fn running(&self) -> bool {
        let c = &self.chats[self.current];
        c.turn.is_some() || c.starting
    }

    /// The last Lua errors in full (empty when the configuration loaded
    /// cleanly).
    pub fn lua_errors(&self) -> &[String] {
        &self.lua.lua_errors
    }

    /// The window gained the focus: look for a saved edit to the Lua
    /// config at once (the window also watches it, `watch_config`). The
    /// first focus only records the hash.
    pub(crate) fn note_focus(&mut self, ctx: &eframe::egui::Context) {
        let focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        let gained = focused && !self.was_focused;
        self.was_focused = focused;
        if gained {
            self.check_config();
        }
    }

    fn check_config(&mut self) {
        if self.config_checking {
            return;
        }
        let Some(dir) = self.opts.config_dir.clone() else {
            return;
        };
        self.config_checking = true;
        let (tx, repaint) = (self.tx.clone(), self.repaint.clone());
        self.rt.spawn(async move {
            let hash = tokio::task::spawn_blocking(move || crate::lua::lua_sources(&dir))
                .await
                .unwrap_or(0);
            let _ = tx.send(Msg::Apply(Box::new(move |gui: &mut Gui| {
                gui.config_checking = false;
                gui.config_seen(hash);
            })));
            wake(&repaint);
        });
    }

    /// The Lua sources hash `hash` now: reload if they changed since last
    /// seen and `bone.o.autoreload` is on. With it off the change is only
    /// noted, so turning it on again compares from then, as in the TUI.
    fn config_seen(&mut self, hash: u64) {
        match self.config_hash {
            Some(prev) if prev != hash => {
                self.config_hash = Some(hash);
                if self.lua.options.autoreload {
                    self.reload_lua();
                }
            }
            None => self.config_hash = Some(hash),
            _ => {}
        }
    }

    /// Watch the Lua sources while the window is open, so a saved edit
    /// reloads without coming back to the window (the window's own loop;
    /// a headless GUI checks on focus only).
    pub(crate) fn watch_config(&self) {
        let Some(dir) = self.opts.config_dir.clone() else {
            return;
        };
        let (tx, repaint) = (self.tx.clone(), self.repaint.clone());
        self.rt.spawn(async move {
            let mut last = None;
            let mut tick = tokio::time::interval(CONFIG_POLL);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                let dir = dir.clone();
                let Ok(hash) =
                    tokio::task::spawn_blocking(move || crate::lua::lua_sources(&dir)).await
                else {
                    continue;
                };
                if last == Some(hash) {
                    continue;
                }
                last = Some(hash);
                let seen = Box::new(move |gui: &mut Gui| gui.config_seen(hash));
                if tx.send(Msg::Apply(seen)).is_err() {
                    return;
                }
                wake(&repaint);
            }
        });
    }
}

/// How often the window looks for saved edits to the Lua sources.
const CONFIG_POLL: std::time::Duration = std::time::Duration::from_millis(250);

fn wake(repaint: &OnceLock<eframe::egui::Context>) {
    if let Some(ctx) = repaint.get() {
        ctx.request_repaint();
    }
}

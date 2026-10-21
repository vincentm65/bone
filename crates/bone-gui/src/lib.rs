//! Desktop bone client.
//!
//! A second frontend beside the TUI, built on the same `bone-ui` state: the
//! transcript reducer, the event handling, and the bare item text are
//! shared; this crate owns the window, drawing, and input. It talks to the
//! core only through a [`bone_client::Client`], so it runs the same against
//! an in-process server or a socket, next to a TUI on the same core.
//!
//! [`Gui`] is the client state and logic, independent of egui; `view.rs`
//! draws it, `native.rs` opens the window, and [`Headless`] drives it
//! without one.

mod headless;
mod native;
mod view;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use bone_client::{Client, Event, EventStream};
use bone_proto::Connection;
use bone_proto::Method;
use bone_proto::methods::{
    AskList, AskRequested, AskRequestedParams, AskResolved, AskRespond, AskRespondParams, Empty,
    QueueAdd, QueueAddParams, QueueMode, SessionCreate, SessionCreateParams, SessionCreated,
    SessionDeleted, SessionList, SessionMessages, SessionRef, SessionUpdated, SettingsChanged,
    SettingsGet,
};
use bone_proto::types::SessionInfo;
use bone_ui::chat::RunningTurn;
use bone_ui::text::{sanitize, wrap};
use serde_json::Value;
use tokio::runtime::Handle;
use tokio::sync::mpsc;

pub use crate::headless::Headless;
pub use crate::native::run;

/// One wrapped row of a chat item, as drawn.
pub type Row = String;
pub type ChatBuffer = bone_ui::chat::ChatBuffer<Row>;

pub struct GuiOptions {
    /// Where new sessions start.
    pub cwd: String,
    /// The socket to connect to again if the server goes away; `None` for
    /// an in-process server, which cannot.
    pub reconnect: Option<PathBuf>,
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
    /// The chat on screen.
    pub current: usize,
    /// The core's sessions, newest first, without owned (sub-agent) ones.
    pub sessions: Vec<SessionInfo>,
    /// Questions waiting for an answer, by id (oldest first).
    pub asks: BTreeMap<u64, AskRequestedParams>,
    pub settings: Value,
    /// The prompt's text.
    pub draft: String,
    pub status: Option<Status>,
    /// The server went away and it is not coming back.
    pub closed: bool,
    /// Width (in columns) the current chat's rows were last wrapped at.
    wrapped_at: usize,
    /// A free-text answer being typed to the open question.
    answer_draft: String,
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
        let gui = Gui {
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
            status: None,
            closed: false,
            wrapped_at: 0,
            answer_draft: String::new(),
        };
        gui.forward(events);
        gui.refresh_sessions();
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
        match msg {
            Msg::Apply(f) => f(self),
            Msg::Event(ev) => self.handle_event(&ev),
            Msg::Lost => self.lost(),
            Msg::Reconnected(r) => self.reconnected(*r),
        }
    }

    /// A server event, as the TUI handles it: transcript events through the
    /// shared reducer, the rest here.
    pub fn handle_event(&mut self, ev: &Event) {
        let chats = &mut self.chats;
        if bone_ui::chat::apply_event(ev, |id| {
            chats.iter_mut().find(|c| c.session_id() == Some(id))
        }) {
            return;
        }
        if let Some(Ok(p)) = ev.parse::<AskRequested>() {
            self.asks.insert(p.ask_id, p);
        } else if let Some(Ok(p)) = ev.parse::<AskResolved>() {
            self.asks.remove(&p.ask_id);
        } else if let Some(Ok(p)) = ev.parse::<SettingsChanged>() {
            self.settings = p.settings;
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

    /// Show session `id`, loading it into a chat the first time.
    pub fn open_session(&mut self, id: &str) {
        if let Some(buf) = self.chat_of(id) {
            self.current = buf;
            return;
        }
        // Reuse an untouched new chat rather than piling them up.
        let c = &self.chats[self.current];
        if !(c.session.is_none() && c.entries.is_empty() && !c.starting) {
            self.chats.push(ChatBuffer::new(None));
            self.current = self.chats.len() - 1;
        }
        let buf = self.current;
        self.load(buf, id.to_owned());
    }

    /// A new, unsaved chat; the session is created by its first message.
    pub fn new_chat(&mut self) {
        let c = &self.chats[self.current];
        if c.session.is_some() || !c.entries.is_empty() {
            self.chats.push(ChatBuffer::new(None));
            self.current = self.chats.len() - 1;
        }
    }

    fn load(&self, buf: usize, session_id: String) {
        self.request::<SessionMessages>(SessionRef { session_id }, move |gui, r| {
            let Some(c) = gui.chats.get_mut(buf) else {
                return;
            };
            match r {
                Ok(r) => {
                    c.load(r.info, &r.messages, r.active_turn);
                    c.set_queue(r.queue, r.queue_paused);
                }
                Err(e) => c.notice(format!("cannot load session: {e}"), true),
            }
        });
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

    fn session_deleted(&mut self, id: &str) {
        if let Some(buf) = self.chat_of(id) {
            self.chats.remove(buf);
            if self.chats.is_empty() {
                self.chats.push(ChatBuffer::new(None));
            }
            if self.current >= buf && self.current > 0 {
                self.current -= 1;
            }
        }
        self.refresh_sessions();
    }

    /// Send the prompt to the chat on screen: it starts a turn on an idle
    /// session and queues otherwise, so it never conflicts with another
    /// client. A new chat's session is created first.
    pub fn submit(&mut self) {
        let text = self.draft.trim().to_owned();
        if text.is_empty() {
            return;
        }
        self.draft.clear();
        let buf = self.current;
        self.chats[buf].starting = true;
        match self.chats[buf].session_id().map(str::to_owned) {
            Some(session_id) => self.queue(buf, session_id, text),
            None => {
                let cwd = Some(self.opts.cwd.clone());
                self.request::<SessionCreate>(SessionCreateParams { cwd }, move |gui, r| match r {
                    Ok(info) => {
                        let session_id = info.session_id.clone();
                        if let Some(c) = gui.chats.get_mut(buf) {
                            c.session = Some(info);
                        }
                        gui.queue(buf, session_id, text);
                    }
                    Err(e) => gui.unsent(buf, text, e),
                });
            }
        }
    }

    fn queue(&self, buf: usize, session_id: String, text: String) {
        let params = QueueAddParams {
            session_id,
            text: text.clone(),
            mode: QueueMode::default(),
            images: Vec::new(),
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
            Err(e) => gui.unsent(buf, text, e),
        });
    }

    /// A prompt that did not go out comes back to the draft.
    fn unsent(&mut self, buf: usize, text: String, e: String) {
        if let Some(c) = self.chats.get_mut(buf) {
            c.starting = false;
        }
        if self.draft.is_empty() {
            self.draft = text;
        }
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
        self.forward(r.events);
        self.info("reconnected to the bone server".into());
    }

    pub fn info(&mut self, text: String) {
        self.status = Some(Status { text, error: false });
    }

    pub fn error(&mut self, text: String) {
        self.status = Some(Status { text, error: true });
    }

    /// Wrap the current chat's stale items at `width` columns, as the TUI
    /// draws them without Lua views, and return its row count.
    pub fn layout_chat(&mut self, width: usize) -> usize {
        let width = width.max(1);
        let chat = &mut self.chats[self.current];
        for (item, index, key) in chat.stale(width, (0, 0), Instant::now()) {
            let data = chat.item_data(item, index, true);
            let rows = bare_rows(&data, width, key.prev.is_none());
            chat.store(item, key, rows, Vec::new(), None, 0);
        }
        self.wrapped_at = width;
        chat.row_count()
    }

    /// The current chat as text, one row per line (after `layout_chat`).
    pub fn transcript(&self) -> String {
        let rows: Vec<&str> = self.chats[self.current]
            .rows()
            .map(String::as_str)
            .collect();
        rows.join("\n")
    }

    /// The current chat's title, for the window.
    pub fn title(&self) -> String {
        self.chats[self.current].title()
    }

    pub fn running(&self) -> bool {
        let c = &self.chats[self.current];
        c.turn.is_some() || c.starting
    }

    /// A question's text, as most hooks write it.
    pub fn question_text(q: &Value) -> String {
        ["title", "text", "message", "prompt"]
            .iter()
            .find_map(|k| q[k].as_str())
            .map_or_else(|| q.to_string(), str::to_owned)
    }

    /// The answers a question offers, if it lists them.
    pub fn question_options(q: &Value) -> Vec<String> {
        ["options", "choices"]
            .iter()
            .find_map(|k| q[k].as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The width the current chat was last wrapped at.
    pub fn wrapped_at(&self) -> usize {
        self.wrapped_at
    }
}

/// Without Lua views: the item's bare text, wrapped, after a blank row
/// unless it comes first. The same rows as the TUI's bare rendering.
fn bare_rows(data: &Value, width: usize, first: bool) -> Vec<Row> {
    let text = bone_ui::chat::bare_text(data);
    if text.is_empty() {
        return Vec::new();
    }
    let gap = (!first).then(String::new);
    gap.into_iter()
        .chain(sanitize(&text).split('\n').flat_map(|l| wrap(l, width)))
        .collect()
}

fn wake(repaint: &OnceLock<eframe::egui::Context>) {
    if let Some(ctx) = repaint.get() {
        ctx.request_repaint();
    }
}

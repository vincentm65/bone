//! RPC transport for the runtime protocol.
//!
//! Carries [`RuntimeEvent`] (core → frontend) and [`RuntimeCommand`]
//! (frontend → core) over a byte stream as newline-delimited JSON. The same
//! `serde` types flow over an in-process channel and over a socket — only the
//! framing differs. (msgpack via `rmpv` could replace the JSONL codec later
//! without touching the protocol types.)
//!
//! Pieces:
//! - [`codec`]: read/write one framed message over any `AsyncRead`/`AsyncWrite`.
//! - [`Hub`]: fan out events to every attached client and merge their commands
//!   into one stream — the multi-client core of `nvim --embed`-style attach.
//! - [`serve_connection`]: glue one client stream to a `Hub`.
//! - [`run_daemon`]: a working headless daemon — each `SubmitPrompt` runs the
//!   agent and streams its events back to all clients.
//!
//! This module is part of core (no `crate::ui`); it compiles ratatui-free.

pub mod codec;

use std::sync::{Arc, Mutex};

use futures_util::future::LocalBoxFuture;
use futures_util::stream::{FuturesUnordered, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::llm::ChatMessage;
use crate::runtime::{RuntimeCommand, RuntimeEvent};

/// Fans [`RuntimeEvent`]s out to all attached clients and merges every client's
/// [`RuntimeCommand`]s into a single receiver the runtime consumes.
#[derive(Clone)]
pub struct Hub {
    events_tx: broadcast::Sender<RuntimeEvent>,
    commands_tx: mpsc::UnboundedSender<RuntimeCommand>,
}

/// Runtime-side half of a [`Hub`]. It can publish events but deliberately does
/// not retain a command sender, so dropping every client closes the command
/// receiver and lets an in-process daemon terminate naturally.
#[derive(Clone)]
pub struct HubPublisher {
    events_tx: broadcast::Sender<RuntimeEvent>,
}

impl HubPublisher {
    /// Broadcast an event to every attached client.
    pub fn publish(&self, event: RuntimeEvent) {
        let _ = self.events_tx.send(event);
    }
}

impl From<Hub> for HubPublisher {
    fn from(hub: Hub) -> Self {
        // Moving out `events_tx` drops `commands_tx` during conversion, so the
        // runtime cannot accidentally keep its own command receiver alive.
        Self {
            events_tx: hub.events_tx,
        }
    }
}

impl Hub {
    /// Create a hub and the single command receiver the runtime reads from.
    pub fn new() -> (Self, mpsc::UnboundedReceiver<RuntimeCommand>) {
        let (events_tx, _) = broadcast::channel(1024);
        let (commands_tx, commands_rx) = mpsc::unbounded_channel();
        (
            Self {
                events_tx,
                commands_tx,
            },
            commands_rx,
        )
    }

    /// Broadcast an event to all attached clients. No-op if none are attached.
    pub fn publish(&self, event: RuntimeEvent) {
        let _ = self.events_tx.send(event);
    }

    /// Return the runtime-facing event publisher without cloning the command
    /// sender. A daemon must own this half rather than [`Hub`] itself or it
    /// would keep its own command channel alive forever.
    pub fn publisher(&self) -> HubPublisher {
        HubPublisher {
            events_tx: self.events_tx.clone(),
        }
    }

    /// Subscribe a new client to the event stream.
    pub fn subscribe(&self) -> broadcast::Receiver<RuntimeEvent> {
        self.events_tx.subscribe()
    }

    /// A sender a client uses to push commands into the merged stream.
    pub fn command_sender(&self) -> mpsc::UnboundedSender<RuntimeCommand> {
        self.commands_tx.clone()
    }

    /// Current attached-client count (event subscribers).
    pub fn client_count(&self) -> usize {
        self.events_tx.receiver_count()
    }
}

/// Which durable conversation a managed TCP connection should attach to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionTarget {
    /// Attach to the daemon's most recently selected conversation.
    Latest,
    /// Create a new durable conversation and attach to it.
    New,
    /// Attach to an existing conversation row.
    Conversation(i64),
}

/// One independently-running conversation created by a session-manager factory.
///
/// The manager retains `hub`, so the actor stays alive when its last browser
/// disconnects. `initial` is evaluated for every attachment, allowing a new
/// client to receive the actor's current transcript/snapshot rather than only
/// its boot-time state.
pub struct ManagedRuntime {
    pub conversation_id: i64,
    pub hub: Hub,
    pub initial: Arc<dyn Fn() -> Vec<RuntimeEvent> + Send + Sync>,
    pub task: LocalBoxFuture<'static, ()>,
}

struct ManagedEntry {
    hub: Hub,
    initial: Arc<dyn Fn() -> Vec<RuntimeEvent> + Send + Sync>,
}

struct SessionAttachment {
    commands: mpsc::UnboundedSender<RuntimeCommand>,
    events: broadcast::Receiver<RuntimeEvent>,
    initial: Vec<RuntimeEvent>,
}

enum SessionRequest {
    Attach {
        target: SessionTarget,
        reply: oneshot::Sender<Result<SessionAttachment, String>>,
    },
}

/// Sendable handle used by TCP connection tasks to attach to conversation
/// actors owned by [`run_session_manager`].
#[derive(Clone)]
pub struct SessionManager {
    requests: mpsc::UnboundedSender<SessionRequest>,
}

impl SessionManager {
    pub fn new() -> (Self, SessionManagerReceiver) {
        let (requests, receiver) = mpsc::unbounded_channel();
        (Self { requests }, SessionManagerReceiver { receiver })
    }

    async fn attach(&self, target: SessionTarget) -> Result<SessionAttachment, String> {
        let (reply, response) = oneshot::channel();
        self.requests
            .send(SessionRequest::Attach { target, reply })
            .map_err(|_| "session manager stopped".to_string())?;
        response
            .await
            .map_err(|_| "session manager stopped".to_string())?
    }
}

/// Runtime-side request receiver. Kept as a distinct type so the public handle
/// remains `Send` while the manager loop may own `!Send` conversation futures.
pub struct SessionManagerReceiver {
    receiver: mpsc::UnboundedReceiver<SessionRequest>,
}

/// Own and concurrently poll one daemon actor per active conversation.
///
/// A factory is called only on the manager's task, so it may construct isolated
/// Lua runtimes and return `!Send` futures. Conversations are keyed by their
/// durable SQLite id; attaching another client to the same id reuses the actor.
pub async fn run_session_manager<F>(mut receiver: SessionManagerReceiver, mut factory: F)
where
    F: FnMut(SessionTarget) -> Result<ManagedRuntime, String>,
{
    let mut sessions = std::collections::HashMap::<i64, ManagedEntry>::new();
    // Tag each actor future with its conversation id so we can evict the map
    // entry when the actor exits (panic, command channel closed, etc.).
    let mut actors = FuturesUnordered::<LocalBoxFuture<'static, (i64, ())>>::new();
    let mut latest_id = None;

    loop {
        tokio::select! {
            request = receiver.receiver.recv() => {
                let Some(SessionRequest::Attach { target, reply }) = request else {
                    break;
                };

                let requested_id = match target {
                    SessionTarget::Conversation(id) => Some(id),
                    SessionTarget::Latest => latest_id,
                    SessionTarget::New => None,
                };
                if let Some(id) = requested_id
                    && let Some(entry) = sessions.get(&id)
                {
                    let _ = reply.send(Ok(SessionAttachment {
                        commands: entry.hub.command_sender(),
                        events: entry.hub.subscribe(),
                        initial: (entry.initial)(),
                    }));
                    latest_id = Some(id);
                    continue;
                }

                match factory(target) {
                    Ok(runtime) => {
                        let id = runtime.conversation_id;
                        // A `Latest` factory may resolve to an actor already in
                        // memory. Prefer the existing owner to prevent two
                        // writers from advancing the same message sequence.
                        if let std::collections::hash_map::Entry::Vacant(entry) =
                            sessions.entry(id)
                        {
                            actors.push(Box::pin(async move { (id, runtime.task.await) }));
                            entry.insert(ManagedEntry {
                                hub: runtime.hub,
                                initial: runtime.initial,
                            });
                        }
                        latest_id = Some(id);
                        let entry = sessions.get(&id).expect("managed session inserted");
                        let _ = reply.send(Ok(SessionAttachment {
                            commands: entry.hub.command_sender(),
                            events: entry.hub.subscribe(),
                            initial: (entry.initial)(),
                        }));
                    }
                    Err(err) => { let _ = reply.send(Err(err)); }
                }
            }
            Some((id, _)) = actors.next(), if !actors.is_empty() => {
                // Actor exited (panic, channel closed, etc.). Evict the stale
                // map entry so a future attach can create a fresh actor for
                // this conversation instead of hitting a dead hub.
                sessions.remove(&id);
            }
        }
    }
}

/// Serve a TCP client whose active event/command channels follow the durable
/// conversation it selects. `LoadConversation` and `NewConversation` are
/// transport-level routing operations here; all other commands go only to the
/// attached conversation actor.
pub async fn serve_managed_connection<S>(
    stream: S,
    manager: SessionManager,
    initial_target: SessionTarget,
) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = codec::MessageReader::new(read_half);
    let mut attachment = manager
        .attach(initial_target)
        .await
        .map_err(std::io::Error::other)?;

    for event in attachment.initial.drain(..) {
        codec::write_message(&mut write_half, &event).await?;
    }

    loop {
        tokio::select! {
            incoming = reader.read::<RuntimeCommand>() => match incoming {
                Some(Ok(RuntimeCommand::LoadConversation { id })) => {
                    match manager.attach(SessionTarget::Conversation(id)).await {
                        Ok(mut next) => {
                            for event in next.initial.drain(..) {
                                codec::write_message(&mut write_half, &event).await?;
                            }
                            attachment = next;
                        }
                        Err(message) => codec::write_message(
                            &mut write_half,
                            &RuntimeEvent::ConversationLoadFailed { id, message },
                        ).await?,
                    }
                }
                Some(Ok(RuntimeCommand::NewConversation)) => {
                    match manager.attach(SessionTarget::New).await {
                        Ok(mut next) => {
                            for event in next.initial.drain(..) {
                                codec::write_message(&mut write_half, &event).await?;
                            }
                            attachment = next;
                        }
                        Err(message) => codec::write_message(
                            &mut write_half,
                            &RuntimeEvent::Status { message },
                        ).await?,
                    }
                }
                Some(Ok(command)) => {
                    if attachment.commands.send(command).is_err() {
                        codec::write_message(
                            &mut write_half,
                            &RuntimeEvent::Status { message: "conversation runtime stopped".into() },
                        ).await?;
                    }
                }
                Some(Err(codec::ReadError::Decode(_))) => continue,
                Some(Err(codec::ReadError::Io(err))) => return Err(err),
                Some(Err(codec::ReadError::TooLong { len })) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "framed message is {len} bytes; max is {}",
                            codec::MAX_LINE_BYTES
                        ),
                    ));
                }
                None => return Ok(()),
            },
            event = attachment.events.recv() => match event {
                Ok(event) => codec::write_message(&mut write_half, &event).await?,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => {
                    codec::write_message(
                        &mut write_half,
                        &RuntimeEvent::Status { message: "conversation runtime stopped".into() },
                    ).await?;
                }
            }
        }
    }
}

/// Client-side counterpart to [`Hub`]: adapts a [`SocketConn`] to a remote
/// `bone serve` into the same `command_sender()` / `subscribe()` interface the
/// in-process [`Hub`] exposes. A frontend can therefore attach to a remote
/// daemon without changing its event loop — it pulls events from a
/// `broadcast::Receiver` and pushes commands to an `UnboundedSender` either way.
///
/// A background task forwards every `next_event()` from the socket into the
/// broadcast channel; when the connection closes, the task ends and the channel
/// closes, surfacing to the frontend as `RecvError::Closed` (same as the daemon
/// dropping).
///
/// The primary receiver is created *before* the forwarder task is spawned and
/// handed to the first `subscribe()` caller. On a multi-thread runtime the
/// spawned task can begin pulling socket events on another worker immediately —
/// before the caller (e.g. `App::with_daemon`, which does synchronous Lua boot
/// work between `connect` and `subscribe`) has subscribed. Registering the
/// receiver up front means the daemon's initial full-state replay is buffered
/// for it rather than broadcast to zero receivers and dropped.
pub struct RemoteClient {
    command_tx: mpsc::UnboundedSender<RuntimeCommand>,
    events_tx: broadcast::Sender<RuntimeEvent>,
    /// Receiver registered at `connect` time, before the forwarder spawns.
    /// Taken by the first `subscribe()`; later subscribers fork fresh ones.
    primary_rx: std::sync::Mutex<Option<broadcast::Receiver<RuntimeEvent>>>,
    /// Owns the socket reader and, transitively, the socket writer task. Kept
    /// here so dropping the bridge can terminate both instead of detaching a
    /// process-lifetime task.
    forwarder: tokio::task::JoinHandle<()>,
}

impl RemoteClient {
    /// Connect over the split halves of a duplex stream to a remote daemon.
    pub fn connect<R, W>(read_half: R, write_half: W) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        use crate::runtime::{RuntimeConn, SocketConn};
        let mut conn = SocketConn::new(read_half, write_half);
        let command_tx = conn.command_sender();
        // Create the primary receiver up front so it's registered before the
        // forwarder can send — otherwise early events race the caller's
        // `subscribe()` and are dropped on a multi-thread runtime.
        let (events_tx, primary_rx) = broadcast::channel(1024);
        let fwd = events_tx.clone();
        let forwarder = tokio::spawn(async move {
            // `send` errors only when there are no receivers; that's fine — an
            // event with no subscriber is simply dropped, like the live Hub.
            while let Some(ev) = conn.next_event().await {
                let _ = fwd.send(ev);
            }
        });
        Self {
            command_tx,
            events_tx,
            primary_rx: std::sync::Mutex::new(Some(primary_rx)),
            forwarder,
        }
    }

    /// A cloneable command sender — same shape as [`Hub::command_sender`].
    pub fn command_sender(&self) -> mpsc::UnboundedSender<RuntimeCommand> {
        self.command_tx.clone()
    }

    /// Subscribe to the daemon's event stream — same shape as [`Hub::subscribe`].
    /// The first call returns the receiver registered at `connect` time (so it
    /// has the buffered initial replay); subsequent calls fork fresh receivers.
    pub fn subscribe(&self) -> broadcast::Receiver<RuntimeEvent> {
        if let Some(rx) = self.primary_rx.lock().unwrap().take() {
            rx
        } else {
            self.events_tx.subscribe()
        }
    }
}

impl Drop for RemoteClient {
    fn drop(&mut self) {
        self.forwarder.abort();
    }
}

/// Build the [`RuntimeEvent::FrontendState`] carrying the daemon-owned resolved
/// settings and extension display metadata for a VM-less frontend.
pub fn frontend_state(
    extensions: &crate::ext::ExtensionManager,
    tools: &crate::tools::registry::ToolHandler,
) -> RuntimeEvent {
    RuntimeEvent::FrontendState {
        banner: extensions.frontend_banner(),
        settings: serde_json::to_value(extensions.frontend_settings()).unwrap_or_default(),
        commands: extensions
            .commands()
            .iter()
            .map(|c| (c.name.clone(), c.description.clone()))
            .collect(),
        tool_defs: tools.definitions(),
        tool_display: serde_json::to_value(tools.display_map()).unwrap_or_default(),
    }
}

/// Serve one client connection against `hub`.
///
/// Late-joiners get `initial` events first (full-state sync), then the live
/// broadcast. Reads run until the client disconnects; writes run until the
/// broadcast closes or the socket errors. Returns when the read side ends.
pub async fn serve_connection<S>(
    stream: S,
    hub: Hub,
    initial: Vec<RuntimeEvent>,
) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let (read_half, write_half) = tokio::io::split(stream);
    let commands_tx = hub.command_sender();
    let mut events_rx = hub.subscribe();

    // Writer task: replay initial state, then stream live events.
    let writer = tokio::spawn(async move {
        let mut w = write_half;
        for ev in initial {
            if codec::write_message(&mut w, &ev).await.is_err() {
                return;
            }
        }
        loop {
            match events_rx.recv().await {
                Ok(ev) => {
                    if codec::write_message(&mut w, &ev).await.is_err() {
                        return;
                    }
                }
                // Dropped messages under backpressure: keep going.
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    });

    // Reader: decode commands until the client disconnects.
    let mut reader = codec::MessageReader::new(read_half);
    while let Some(result) = reader.read::<RuntimeCommand>().await {
        match result {
            Ok(cmd) => {
                if commands_tx.send(cmd).is_err() {
                    break; // runtime gone
                }
            }
            // Skip malformed frames rather than dropping the connection.
            Err(codec::ReadError::Decode(_)) => continue,
            Err(codec::ReadError::Io(e)) => {
                writer.abort();
                return Err(e);
            }
            Err(codec::ReadError::TooLong { len }) => {
                writer.abort();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "framed message is {len} bytes; max is {}",
                        codec::MAX_LINE_BYTES
                    ),
                ));
            }
        }
    }

    writer.abort();
    Ok(())
}

/// The disposition of an idle-state [`RuntimeCommand`]: either it was fully
/// handled and the loop should wait for the next one (`Continue`), or it asks
/// the runtime to run a model turn with the given prompt text (`StartTurn`).
/// `SubmitPrompt` and a *submitting* `RunCommand` are the only commands that
/// start a turn; every other command is `Continue`.
enum Flow {
    Continue,
    StartTurn(String),
}

/// The daemon's shared state, threaded through command handling so each
/// command's behavior lives in exactly one place (instead of being re-coded in
/// the idle dispatch, the mid-turn select, and the interactive-command loop).
/// `llm` and `extensions` are owned here because `SwitchProvider` /
/// `ReloadExtensions` reassign them in place; the registries and `mode` are
/// shared with the in-flight turn.
struct DaemonCtx {
    hub: HubPublisher,
    llm: Arc<dyn crate::llm::provider::LlmProvider>,
    extensions: crate::ext::ExtensionManager,
    session: Arc<Mutex<crate::runtime::RuntimeSession>>,
    mode: crate::tools::SharedApprovalMode,
    approval_registry: crate::runtime::ApprovalReplyRegistry,
    key_registry: crate::runtime::KeyReplyRegistry,
    // In-process hand-off for `ReloadExtensions`. When a frontend shares the
    // Lua VM with the daemon (the in-process TUI), it boots the extensions
    // once and drops the cloned result here, letting the daemon adopt it
    // instead of re-reading disk and booting a second VM. `None` (e.g. `bone
    // serve`) falls back to booting from disk.
    reload_inbox: Option<Arc<Mutex<Option<crate::ext::BootedTools>>>>,
    // Forward Lua `ViewDiff`s (pane/UI updates) as `RuntimeEvent::ViewDiff` so a
    // *remote* frontend renders them. The in-process TUI shares the VM and
    // drains the `UiState` itself, so it passes `false` to avoid a double-drain
    // race; `bone serve` passes `true`.
    forward_view_diffs: bool,
}

impl DaemonCtx {
    /// Publish a `StateSnapshot` derived from the current session + provider.
    /// Swap the active provider (and model) to the given ids, e.g. when loading
    /// a conversation that was created with a different provider. A no-op when
    /// already matching. Failure keeps the current provider — the caller still
    /// snapshots so the frontend proceeds with the old provider label.
    fn restore_provider(&mut self, provider_id: &str, model: &str) {
        if self.llm.id() == provider_id && self.llm.model() == model {
            return;
        }
        let custom = crate::config::custom::CustomConfigs::load();
        let providers_config = custom.derive_providers_config();
        match crate::llm::providers::build_provider(provider_id, model, &providers_config) {
            Ok(new_provider) => self.llm = Arc::from(new_provider),
            Err(err) => self.hub.publish(RuntimeEvent::Status {
                message: format!("failed to restore provider `{provider_id}`: {err}"),
            }),
        }
    }

    fn publish_snapshot(&self) {
        self.hub.publish(RuntimeEvent::StateSnapshot {
            snapshot: {
                let s = self.session.lock().unwrap();
                s.snapshot(self.llm.id(), self.llm.model())
            },
        });
    }

    /// Forward any pane/UI diffs the Lua VM has queued to remote frontends.
    fn drain_diffs(&self) {
        for diff in self.extensions.drain_view_diffs() {
            self.hub.publish(RuntimeEvent::ViewDiff { diff });
        }
    }

    /// Drop conversation-scoped host tool state (task_list, …) and remove the
    /// task_list pane. Used on `/new`, `/clear`, and conversation load so
    /// checklists never leak across chats.
    fn reset_host_tool_state(&self) {
        {
            let mut s = self.session.lock().unwrap();
            s.tools.clear_host_state();
        }
        let ui = self.extensions.ui_handle();
        crate::ext::api_ui::lock_shared(&ui).apply(crate::runtime::ViewDiff::Remove {
            id: "task_list".into(),
        });
        if self.forward_view_diffs {
            self.drain_diffs();
        }
    }

    /// Apply a Safe/Danger toggle. The gate reads the shared atomic per call, so
    /// this takes effect immediately — even mid-turn. Unknown values are
    /// rejected (not silently coerced to Safe) so a bad setting/client is
    /// visible.
    fn set_mode(&self, mode_str: &str) -> bool {
        match crate::tools::ApprovalMode::parse(mode_str) {
            Ok(mode) => {
                self.mode.set(mode);
                true
            }
            Err(err) => {
                self.hub.publish(RuntimeEvent::Status { message: err });
                false
            }
        }
    }

    fn persist_mode(&self, mode_str: &str) {
        if !self.set_mode(mode_str) {
            return;
        }
        let result = crate::config::settings::Settings::load().and_then(|settings| {
            let mut settings = settings.ok_or_else(|| {
                crate::config::settings::SettingsError::Validation(
                    "config.yaml does not exist".into(),
                )
            })?;
            settings.set_value("general", "approval_mode", mode_str.to_string())?;
            Ok(settings.into_resolved())
        });
        match result {
            Ok(settings) => {
                self.extensions.replace_settings(settings);
                self.hub.publish(frontend_state(
                    &self.extensions,
                    &self.session.lock().unwrap().tools,
                ));
            }
            Err(err) => self.hub.publish(RuntimeEvent::Status {
                message: format!("could not save approval mode: {err}"),
            }),
        }
    }

    /// Reload only the canonical settings file. This is the safe `/config`
    /// apply path: extension/tool/provider state is left untouched, while all
    /// frontend-owned settings and the approval gate update atomically.
    fn reload_settings(&self) {
        match crate::config::settings::Settings::load() {
            Ok(Some(settings)) => {
                let resolved = settings.into_resolved();
                self.set_mode(&resolved.general.approval);
                self.extensions.replace_settings(resolved);
                self.hub.publish(frontend_state(
                    &self.extensions,
                    &self.session.lock().unwrap().tools,
                ));
            }
            Ok(None) => self.hub.publish(RuntimeEvent::Status {
                message: "settings reload failed: config.yaml does not exist".into(),
            }),
            Err(err) => self.hub.publish(RuntimeEvent::Status {
                message: format!("settings reload failed: {err}"),
            }),
        }
    }

    /// Sync terminal width from the frontend so Lua panes wrap correctly.
    fn set_width(&self, width: u16) {
        let ui_handle = self.extensions.ui_handle();
        let mut ui = ui_handle.lock().unwrap_or_else(|e| e.into_inner());
        ui.terminal_width = width;
    }

    /// Fire-and-forget Lua hook on the daemon's VM.
    fn dispatch_hook(&self, name: String, payload: serde_json::Value) {
        self.extensions.dispatch_simple(&name, payload);
    }

    /// Terminate every running background sub-agent for this session, surfacing
    /// a notice when any were actually cancelled. Called on turn cancel (Ctrl+C)
    /// and on conversation reset (`/new`, `/clear`).
    fn cancel_jobs(&self) {
        // Scope to this session's conversation so a process hosting several
        // conversations (`bone serve`) doesn't kill another one's sub-agents.
        let scope = self.session.lock().unwrap().conversation_id;
        let cancelled = crate::ext::jobs::registry().cancel_all_scoped(scope);
        if cancelled > 0 {
            self.hub.publish(RuntimeEvent::Status {
                message: format!("cancelled {cancelled} background sub-agent job(s)"),
            });
        }
    }

    /// Next queued background prompt to inject as a turn when the daemon is idle,
    /// or `None` when nothing is pending. Lua-submitted prompts (`bone.submit`)
    /// go first, one per idle tick; otherwise a batch of this conversation's
    /// finished sub-agent jobs is formatted into a single turn and marked
    /// consumed so it is never injected twice. Only called when
    /// `inject_background` is set (i.e. `bone serve`).
    fn next_background_prompt(&self) -> Option<String> {
        // `bone.submit` prompts first — steering should win over passively
        // arriving job results.
        if let Some(text) = crate::ext::inbox::pop() {
            return Some(text);
        }
        let scope = self.session.lock().unwrap().conversation_id;
        let registry = crate::ext::jobs::registry();
        let finished = registry.peek_finished_unconsumed_scoped(scope);
        if finished.is_empty() {
            return None;
        }
        let running = registry.running_jobs_scoped(scope);
        let (turn_text, _display) =
            crate::ext::jobs::format_results_for_injection(&finished, &running)?;
        let ids: Vec<String> = finished.iter().map(|j| j.id.clone()).collect();
        registry.mark_consumed(&ids);
        Some(turn_text)
    }

    /// Run a registered Lua slash command inside the daemon, forwarding its pane
    /// diffs (`ViewDiff`) and key requests (`KeyRequest`) to clients and pumping
    /// `KeyReply`/`Cancel` back, exactly like a turn.
    ///
    /// Returns:
    /// - `None` — the command name isn't registered (a genuine "unknown command").
    /// - `Some(None)` — the handler ran but returned a no-op (e.g. `{ submit =
    ///   false }` with no output/action, which [`parse_lua_command_return`] maps to
    ///   `None`). This must NOT be reported as "unknown command"; the command was
    ///   handled and simply has nothing to submit. Mirrors the local TUI path, where
    ///   a handler-found-but-no-op result is treated as "handled, just redraw".
    /// - `Some(Some(ret))` — the handler ran and produced output/an action.
    ///
    /// [`parse_lua_command_return`]: crate::ext::types::parse_lua_command_return
    ///
    /// This is the daemon-side equivalent of the TUI's `run_lua_command`: it lets a
    /// remote frontend run interactive commands against the daemon's Lua VM.
    /// The pure-client TUI also routes slash commands over this path.
    async fn run_interactive_command(
        &self,
        commands: &mut mpsc::UnboundedReceiver<RuntimeCommand>,
        name: String,
        input: String,
    ) -> Option<(
        Option<crate::ext::types::LuaCommandReturn>,
        Vec<crate::ext::ctx::ConversationOperation>,
    )> {
        use std::sync::atomic::{AtomicBool, Ordering};

        // App-derived ctx snapshot, assembled from the session + provider the same
        // way the TUI's `app_ctx_state` does.
        let app_state = {
            let s = self.session.lock().unwrap();
            let by_provider = crate::ext::ctx::usage_by_provider_context(
                s.session_db.as_ref(),
                s.conversation_id,
            );
            crate::ext::ctx::AppCtxState::new(
                &s.tools,
                &s.token_stats,
                &self.mode.get(),
                s.conversation_id,
                self.llm.id(),
                self.llm.model(),
                None,
                by_provider,
                s.transcript.clone(),
                s.turn_nudge.lock().unwrap().clone(),
            )
        };

        let lua = self.extensions.lua_handle();
        let shared_ui = self.extensions.ui_handle();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_for_ctx = cancel.clone();
        let (live_tx, mut live_rx) = mpsc::unbounded_channel::<crate::pane_content::KeyRequest>();
        let (conversation_tx, conversation_rx) = std::sync::mpsc::channel();

        // The handler call blocks (Lua + nested tool calls), so run it off the
        // async runtime. Mirrors the TUI's spawn_blocking in `run_lua_command`.
        // Outer `Option` = "was the command found?"; inner `Option` = the parsed
        // result (a found handler may legitimately return a no-op `None`).
        let mut handle = tokio::task::spawn_blocking(move || {
            let lua_guard = lua.lock().unwrap_or_else(|e| e.into_inner());
            // Not found: the only case that should surface as "unknown command".
            let handler = crate::ext::ops_commands::find_handler(&lua_guard, &name)?;
            let config_dir = crate::config::bone_dir().to_string_lossy().to_string();
            let shared_state = app_state.tool_handler.shared_state.clone();
            let mut ctx_cfg = crate::ext::ctx::CtxConfig::new(config_dir, shared_state);
            app_state.apply_to(&mut ctx_cfg);
            ctx_cfg.key_sender = Some(live_tx);
            ctx_cfg.ui = Some(shared_ui);
            ctx_cfg.cancelled = Some(cancel_for_ctx);
            ctx_cfg.conversation_operations = Some(conversation_tx);
            // The handler exists; from here every outcome is `Some(_)` so the daemon
            // never mistakes a ran command for an unknown one.
            let ctx_table = match crate::ext::ctx::create_ctx_table(&lua_guard, &ctx_cfg) {
                Ok(t) => t,
                Err(_) => return Some((None, Vec::new())),
            };
            // Release the VM lock before calling in: a nested `ctx.tools.call` runs
            // inline on this thread and must re-acquire the (non-reentrant) mutex.
            drop(lua_guard);
            let ret = match handler.call::<mlua::Value>((input, ctx_table)) {
                Ok(value) => crate::ext::types::parse_lua_command_return(value),
                Err(e) => Some(crate::ext::types::LuaCommandReturn {
                    output: format!("Lua command error: {e}"),
                    submit: false,
                    action: None,
                    display_role: None,
                }),
            };
            Some((ret, conversation_rx.try_iter().collect()))
        });

        let mut diff_timer = tokio::time::interval(std::time::Duration::from_millis(50));
        loop {
            tokio::select! {
                res = &mut handle => {
                    // Flush any trailing pane diffs the handler emitted.
                    self.drain_diffs();
                    return res.ok().flatten();
                }
                Some(req) = live_rx.recv() => {
                    let id = self.key_registry.register(req);
                    self.hub.publish(RuntimeEvent::KeyRequest { id });
                }
                _ = diff_timer.tick() => self.drain_diffs(),
                cmd = commands.recv() => match cmd {
                    Some(RuntimeCommand::KeyReply { id, key }) => { self.key_registry.resolve(id, key); }
                    Some(RuntimeCommand::Cancel) | None => {
                        // Signal the blocking handler, then stop waiting for
                        // it. Cooperative handlers/tools poll this flag and
                        // self-abort; detaching the handle ensures we don't
                        // wedge on a non-cooperative one (which would ignore
                        // every subsequent command). The caller publishes an
                        // empty CommandComplete, like the no-op path.
                        cancel.store(true, Ordering::Relaxed);
                        self.drain_diffs();
                        return Some((None, Vec::new()));
                    }
                    Some(_) => {} // other commands are ignored while a command runs
                },
            }
        }
    }

    fn load_conversation(&mut self, id: i64) {
        let loaded = {
            let s = self.session.lock().unwrap();
            s.session_db.as_ref().and_then(|db| {
                let full = db.load_messages(id).ok()?;
                let effective = db.load_effective_transcript(id).ok()?;
                let provider_model = db.conversation_provider_model(id).ok().flatten();
                Some((full, effective, provider_model))
            })
        };
        if let Some((rows, effective, provider_model)) = loaded {
            if let Some((provider_id, model)) = provider_model {
                self.restore_provider(&provider_id, &model);
            }
            let messages = rows
                .into_iter()
                .map(crate::session_db::stored_to_chat_message)
                .collect::<Vec<_>>();
            let snapshot = {
                let mut s = self.session.lock().unwrap();
                if let Some(db) = s.session_db.as_ref() {
                    if let Some(old) = s.conversation_id
                        && old != id
                    {
                        let _ = db.end_conversation(old);
                    }
                    let _ = db.reopen_conversation(id);
                }
                s.conversation_id = Some(id);
                s.session_seq = s
                    .session_db
                    .as_ref()
                    .and_then(|db| db.max_message_seq(id).ok())
                    .unwrap_or(0);
                s.transcript = effective;
                s.restore_usage_and_context();
                s.snapshot(self.llm.id(), self.llm.model())
            };
            self.reset_host_tool_state();
            self.hub
                .publish(RuntimeEvent::ConversationLoaded { messages, snapshot });
        } else {
            self.hub.publish(RuntimeEvent::ConversationLoadFailed {
                id,
                message: format!("failed to load conversation {id}"),
            });
        }
    }

    /// Handle one command received while the runtime is idle. Returns [`Flow`]:
    /// `Continue` once the command is fully serviced, or `StartTurn(text)` when
    /// the command should run a model turn (`SubmitPrompt`, or a `RunCommand`
    /// whose handler asked to submit its output). `commands` is borrowed so an
    /// interactive `RunCommand` can pump replies while its handler runs.
    async fn handle_idle_command(
        &mut self,
        cmd: RuntimeCommand,
        commands: &mut mpsc::UnboundedReceiver<RuntimeCommand>,
    ) -> Flow {
        match cmd {
            RuntimeCommand::SubmitPrompt { text, images } => {
                // Push the user message to the transcript + DB before building
                // the driver. The Driver detects the duplicate (last message is
                // already the user prompt) and skips its own push; images are
                // embedded in the transcript entry the driver builds history
                // from. This mirrors the TUI's pre-turn push.
                let images_json = if images.is_empty() {
                    None
                } else {
                    serde_json::to_string(&images).ok()
                };
                {
                    let mut s = self.session.lock().unwrap();
                    if images.is_empty() {
                        s.transcript
                            .push(ChatMessage::new(crate::llm::ChatRole::User, &text));
                    } else {
                        s.transcript
                            .push(ChatMessage::user_with_images(&text, images));
                    }
                    s.append_user_to_db(&text, images_json.as_deref());
                }
                self.extensions.dispatch_simple(
                    "message",
                    serde_json::json!({ "role": "user", "content": text }),
                );
                Flow::StartTurn(text)
            }
            // ── Lifecycle commands (idle only) ──────────────────────────
            RuntimeCommand::NewConversation => {
                // Resetting the conversation also ends its background
                // sub-agents — they belong to the conversation being left.
                self.cancel_jobs();
                {
                    let mut s = self.session.lock().unwrap();
                    // Already on an empty conversation? Reuse it instead of
                    // stacking another empty row (and publish a fresh snapshot
                    // below so the client still resets its view).
                    let already_empty = s.transcript.is_empty() && s.session_seq == 0;
                    if !already_empty && let Some(db) = s.session_db.as_ref() {
                        if let Some(conv_id) = s.conversation_id {
                            let _ = db.end_conversation(conv_id);
                        }
                        match db.create_conversation(self.llm.id(), self.llm.model()) {
                            Ok(conv_id) => {
                                s.conversation_id = Some(conv_id);
                                s.session_seq = 0;
                            }
                            Err(err) => {
                                self.hub.publish(RuntimeEvent::Status {
                                    message: format!("failed to create conversation: {err}"),
                                });
                                return Flow::Continue;
                            }
                        }
                    }
                    s.transcript.clear();
                    s.token_stats.reset();
                }
                self.reset_host_tool_state();
                self.publish_snapshot();
                Flow::Continue
            }
            RuntimeCommand::LoadConversation { id } => {
                self.load_conversation(id);
                Flow::Continue
            }
            RuntimeCommand::SetApprovalMode { mode: mode_str } => {
                self.persist_mode(&mode_str);
                Flow::Continue
            }
            RuntimeCommand::AppendMessage { role, content } => {
                // Locally-produced context (inline `!command` output) folded into
                // the transcript so the next turn's history includes it.
                let chat_role = match role.as_str() {
                    "assistant" => crate::llm::ChatRole::Assistant,
                    "system" => crate::llm::ChatRole::System,
                    _ => crate::llm::ChatRole::User,
                };
                let mut s = self.session.lock().unwrap();
                s.transcript.push(ChatMessage::new(chat_role, &content));
                // Persist so the folded context survives a reload / daemon
                // restart, like the SubmitPrompt path's `append_user_to_db`.
                // Without this the next turn captures `persist_from` past this
                // message, so it is never written to the DB.
                s.append_db_message(&role, &content, None, None, None, None);
                Flow::Continue
            }
            RuntimeCommand::ClearConversation => {
                self.cancel_jobs();
                {
                    let mut s = self.session.lock().unwrap();
                    s.transcript.clear();
                    s.token_stats.reset();
                }
                self.reset_host_tool_state();
                self.publish_snapshot();
                Flow::Continue
            }
            RuntimeCommand::ReplaceConversation { messages } => {
                {
                    let mut s = self.session.lock().unwrap();
                    s.transcript = messages;
                    if let (Some(db), Some(conv_id)) = (s.session_db.as_ref(), s.conversation_id) {
                        let _ = db.save_context_checkpoint(conv_id, s.session_seq, &s.transcript);
                    }
                    let history = crate::chat::build_chat_history(&s.transcript, None);
                    let tool_defs_json_chars = serde_json::to_value(s.tools.definitions())
                        .map(|v| v.to_string().chars().count())
                        .unwrap_or(0);
                    let prompt_chars =
                        crate::agent::estimate_context_chars(&history, tool_defs_json_chars);
                    s.token_stats.set_context_estimate(prompt_chars);
                }
                self.publish_snapshot();
                Flow::Continue
            }
            RuntimeCommand::SwitchProvider { provider_id } => {
                let custom = crate::config::custom::CustomConfigs::load();
                let providers_config = custom.derive_providers_config();
                match crate::llm::providers::create_provider_with_config(
                    &provider_id,
                    &providers_config,
                ) {
                    Ok(new_provider) => {
                        self.llm = Arc::from(new_provider);
                        // Keep the current conversation's stored provider/model in
                        // step with the active provider, so the sidebar and the
                        // reopen path (restore_provider) reflect this choice rather
                        // than the default the row was minted with.
                        let s = self.session.lock().unwrap();
                        if let (Some(db), Some(conv_id)) =
                            (s.session_db.as_ref(), s.conversation_id)
                        {
                            let _ = db.set_conversation_provider(
                                conv_id,
                                self.llm.id(),
                                self.llm.model(),
                            );
                        }
                    }
                    Err(err) => self.hub.publish(RuntimeEvent::Status {
                        message: format!("failed to switch provider: {err}"),
                    }),
                }
                // Always snapshot, even on failure (keeping the old provider), so
                // the frontend's `await_state_snapshot` unblocks instead of
                // hanging forever waiting on a snapshot that never comes.
                self.publish_snapshot();
                Flow::Continue
            }
            RuntimeCommand::ReloadSettings => {
                self.reload_settings();
                Flow::Continue
            }
            RuntimeCommand::ReloadExtensions => {
                // An in-process frontend boots the extensions itself and leaves
                // the cloned result in the inbox; adopt it (shared Lua VM, no
                // disk read). Otherwise boot from disk.
                let booted = match self
                    .reload_inbox
                    .as_ref()
                    .and_then(|m| m.lock().unwrap().take())
                {
                    Some(booted) => booted,
                    None => {
                        let config_dir = crate::config::bone_dir();
                        let cwd = std::env::current_dir().unwrap_or_default();
                        let mut custom = crate::config::custom::CustomConfigs::load();
                        let model = self.llm.model().to_string();
                        let provider = format!("{} ({})", self.llm.name(), self.llm.id());
                        crate::ext::boot_with_tools(
                            &config_dir,
                            &cwd,
                            &mut custom,
                            true,
                            crate::ext::BootOptions {
                                agent_depth: 0,
                                headless: true,
                                model: model.clone(),
                                provider: provider.clone(),
                                tool_allowlist: None,
                            },
                            &model,
                            &provider,
                        )
                    }
                };
                self.extensions = booted.manager;
                {
                    let mut s = self.session.lock().unwrap();
                    // Reloading extensions must not wipe conversation-scoped
                    // tool state. The new registry has fresh definitions, but
                    // snapshots, host state (task_list, …), gates, and cancel
                    // tokens belong to the session and must survive the swap.
                    let mut tools = booted.tools;
                    tools.adopt_session_state_from(&s.tools);
                    s.tools = tools;
                }
                let count = self.session.lock().unwrap().tools.definitions().len();
                self.hub.publish(RuntimeEvent::Status {
                    message: format!("Tools and Lua extensions reloaded. {count} tools enabled."),
                });
                // Re-ship display state: a reload can change theme/keymap/banner/
                // commands/tools, and a VM-less frontend has no other way to
                // learn them.
                self.hub.publish(frontend_state(
                    &self.extensions,
                    &self.session.lock().unwrap().tools,
                ));
                self.publish_snapshot();
                Flow::Continue
            }
            RuntimeCommand::RunCommand { name, input } => {
                let result = self
                    .run_interactive_command(commands, name.clone(), input)
                    .await;
                let (ret, operations) = match result {
                    // Command name isn't registered: the only genuine "unknown".
                    None => {
                        self.hub.publish(RuntimeEvent::Status {
                            message: format!("unknown command: {name}"),
                        });
                        self.hub.publish(RuntimeEvent::CommandComplete {
                            output: String::new(),
                            submit: false,
                            display_role: None,
                            action: None,
                        });
                        return Flow::Continue;
                    }
                    Some(result) => result,
                };
                let Some(ret) = ret else {
                    self.hub.publish(RuntimeEvent::CommandComplete {
                        output: String::new(),
                        submit: false,
                        display_role: None,
                        action: None,
                    });
                    let has_operations = !operations.is_empty();
                    for operation in operations {
                        match operation {
                            crate::ext::ctx::ConversationOperation::Load(id) => {
                                self.load_conversation(id)
                            }
                        }
                    }
                    if !has_operations {
                        self.publish_snapshot();
                    }
                    return Flow::Continue;
                };
                // Forward any config/runtime/conversation action the handler
                // requested. These are frontend-coupled (local config state,
                // rendered scrollback), so the client applies them on receipt
                // via `App::apply_lua_action`; the daemon only carries them.
                // A reply-bearing action (config_action) yields a status
                // reply ("Switched to …", "Configuration applied.") that must
                // be displayed, not submitted as a user turn. Force submit=false
                // so the RPC path can't diverge from the local path.
                let reply_bearing = ret
                    .action
                    .as_ref()
                    .and_then(|a| a.config_action.as_ref())
                    .is_some();
                // A conversation switch and an immediate submitted turn cannot be
                // represented as one command completion. Let the switch win rather
                // than telling the frontend to wait for a turn that will not start.
                let submit = ret.submit && !reply_bearing && operations.is_empty();
                let action = ret.action.as_ref().and_then(|a| a.to_command_action());
                let output = if ret.output.is_empty() {
                    match ret.action.as_ref().and_then(|a| a.config_action.as_ref()) {
                        Some(crate::ext::types::ConfigAction::Apply) => {
                            "Configuration applied.".to_string()
                        }
                        Some(crate::ext::types::ConfigAction::ApplyRestartRequired) => {
                            "Configuration saved. Restart required for tool/command changes."
                                .to_string()
                        }
                        _ => String::new(),
                    }
                } else {
                    ret.output.clone()
                };
                self.hub.publish(RuntimeEvent::CommandComplete {
                    output,
                    submit,
                    display_role: ret.display_role.clone(),
                    action,
                });
                if !operations.is_empty() {
                    for operation in operations {
                        match operation {
                            crate::ext::ctx::ConversationOperation::Load(id) => {
                                self.load_conversation(id)
                            }
                        }
                    }
                    return Flow::Continue;
                }
                if submit && !ret.output.is_empty() {
                    // Submit the handler's output as the next turn (mirrors the
                    // SubmitPrompt pre-turn push), then run the turn.
                    {
                        let mut s = self.session.lock().unwrap();
                        s.transcript
                            .push(ChatMessage::new(crate::llm::ChatRole::User, &ret.output));
                        s.append_user_to_db(&ret.output, None);
                    }
                    self.extensions.dispatch_simple(
                        "message",
                        serde_json::json!({ "role": "user", "content": ret.output }),
                    );
                    Flow::StartTurn(ret.output)
                } else {
                    self.publish_snapshot();
                    Flow::Continue
                }
            }
            RuntimeCommand::KeymapDispatch { action } => {
                let kind = self.extensions.dispatch_keymap(&action);
                self.hub.publish(RuntimeEvent::KeymapDispatched { kind });
                Flow::Continue
            }
            // Fire-and-forget Lua hook on the daemon's VM.
            RuntimeCommand::DispatchHook { name, payload } => {
                self.dispatch_hook(name, payload);
                Flow::Continue
            }
            // Sync terminal width from the frontend so Lua panes wrap correctly.
            RuntimeCommand::SetTerminalWidth { width } => {
                self.set_width(width);
                Flow::Continue
            }
            // A cancel while idle has no turn to stop, but background
            // sub-agents may still be running — terminate them.
            RuntimeCommand::Cancel => {
                self.cancel_jobs();
                Flow::Continue
            }
            // Acknowledge other non-turn commands so a client isn't left waiting.
            other => {
                self.hub.publish(RuntimeEvent::Status {
                    message: format!("ignored (idle): {other:?}"),
                });
                Flow::Continue
            }
        }
    }

    /// Build and pump one model turn for `text`. A [`LocalConn`] runs the Driver
    /// on this task (the Lua VM is `!Send`, so the turn is never spawned); the
    /// command stream keeps flowing so `ApprovalReply`/`KeyReply`/`Cancel` route
    /// into the turn and a mid-turn `SetApprovalMode`/width/hook still applies
    /// (via the same shared mutators the idle path uses). After it drains, the
    /// session reabsorbs the outcome and a fresh `StateSnapshot` is published.
    async fn run_turn(&self, text: String, commands: &mut mpsc::UnboundedReceiver<RuntimeCommand>) {
        use crate::runtime::{ChannelApprovalGate, LocalConn, RuntimeConn};
        use std::sync::atomic::AtomicBool;

        let (rt_tx, rt_rx) = mpsc::unbounded_channel::<RuntimeEvent>();
        let cancel = Arc::new(AtomicBool::new(false));
        let work_timer = crate::runtime::timer::WorkTimer::start();
        self.key_registry.set_timer(Some(work_timer.clone()));
        let working_dir = self.session.lock().unwrap().tools.working_dir.clone();
        let gate = Arc::new(ChannelApprovalGate::new(
            rt_tx.clone(),
            self.approval_registry.clone(),
            Some(work_timer.clone()),
            working_dir,
        ));
        let driver = {
            let s = self.session.lock().unwrap();
            s.build_driver(
                self.llm.clone(),
                self.extensions.clone(),
                self.mode.clone(),
                gate,
                rt_tx.clone(),
                self.key_registry.clone(),
                cancel.clone(),
                Arc::new(crate::session_sink::NullSessionSink),
            )
        };
        let mut conn = LocalConn::new(
            rt_rx,
            rt_tx,
            driver,
            cancel,
            self.approval_registry.clone(),
            self.key_registry.clone(),
            self.session.lock().unwrap().turn_nudge.clone(),
        );
        conn.send(RuntimeCommand::SubmitPrompt {
            text,
            images: vec![],
        });

        // Pump the turn: publish its events, and concurrently route interactive
        // replies (and cancel) from any client back into the running turn. When
        // forwarding is on, a timer drains the VM's `UiState` and forwards pane
        // diffs as events (the in-process TUI drains the shared handle itself).
        let mut diff_timer = tokio::time::interval(std::time::Duration::from_millis(50));
        loop {
            tokio::select! {
                ev = conn.next_event() => match ev {
                    Some(ev) => self.hub.publish(ev),
                    None => break, // turn drained
                },
                _ = diff_timer.tick(), if self.forward_view_diffs => self.drain_diffs(),
                cmd = commands.recv() => match cmd {
                    // A turn cancel also terminates the session's background
                    // sub-agents: they were spawned by this conversation, so
                    // Ctrl+C should stop them too rather than leave them running
                    // and injecting results into a turn the user abandoned.
                    Some(cmd @ RuntimeCommand::Cancel) => {
                        self.cancel_jobs();
                        conn.send(cmd);
                    }
                    Some(cmd @ (RuntimeCommand::ApprovalReply { .. }
                    | RuntimeCommand::KeyReply { .. })) => conn.send(cmd),
                    // Mid-turn Safe/Danger toggle: applies to the rest of the turn
                    // (the gate reads the shared atomic per call).
                    Some(RuntimeCommand::SetApprovalMode { mode: mode_str }) => self.persist_mode(&mode_str),
                    Some(RuntimeCommand::ReloadSettings) => self.reload_settings(),
                    // A second submit mid-turn is dropped (the runtime is busy
                    // running one turn at a time). Tell the client so it isn't
                    // left waiting on a prompt that will never run, mirroring the
                    // idle-path acknowledgement.
                    Some(RuntimeCommand::SubmitPrompt { .. }) => self.hub.publish(RuntimeEvent::Status {
                        message: "busy: a turn is in progress; prompt ignored".into(),
                    }),
                    // Width updates and hooks are safe mid-turn.
                    Some(RuntimeCommand::SetTerminalWidth { width }) => self.set_width(width),
                    Some(RuntimeCommand::DispatchHook { name, payload }) => self.dispatch_hook(name, payload),
                    Some(cmd @ RuntimeCommand::Steer { .. }) => conn.send(cmd),
                    Some(_) => {}
                    None => break,
                },
            }
        }
        // Flush any diffs emitted between the last tick and turn end.
        if self.forward_view_diffs {
            self.drain_diffs();
        }

        // Drop any steer that wasn't consumed before the turn ended (e.g. sent
        // during the model's final, tool-call-free round, so the driver loop
        // never reached another top-of-iteration `take`). The nudge Arc is
        // session-lived and shared across turns, so a leftover would otherwise
        // leak into the *next* unrelated turn.
        {
            let session = self.session.lock().unwrap();
            if session.turn_nudge.lock().unwrap().take().is_some() {
                self.hub.publish(RuntimeEvent::Status {
                    message: "steer not applied — the turn had already finished".into(),
                });
            }
        }

        if let Some(outcome) = conn.take_outcome() {
            let _ = self.session.lock().unwrap().apply_outcome(outcome);
        }
        // Publish the post-turn state so clients can sync their view-model.
        self.key_registry.set_timer(None);
        self.publish_snapshot();
        self.hub.publish(RuntimeEvent::WorkElapsed {
            elapsed_ms: work_timer.elapsed_ms(),
        });
        self.hub.publish(RuntimeEvent::TurnComplete);
    }
}

/// The persistent headless runtime: owns one [`RuntimeSession`] across turns and
/// drives each [`RuntimeCommand::SubmitPrompt`] to completion, broadcasting the
/// turn's [`RuntimeEvent`]s to every attached client.
///
/// Interaction (tool approval, `ctx.ui.key`) works over the wire: a turn runs
/// through a [`LocalConn`] on this task (the Lua VM is `!Send`, so the turn is
/// never spawned), while the daemon keeps reading the merged command stream and
/// routes `ApprovalReply` / `KeyReply` / `Cancel` into the connection. After the
/// turn, the session reabsorbs the outcome (transcript/token-stats/tool-state +
/// DB persistence) so the next turn — and any newly attached client — sees the
/// accumulated conversation. This is the server half of "the TUI is a client".
#[allow(clippy::too_many_arguments)]
pub async fn run_daemon(
    hub: impl Into<HubPublisher>,
    mut commands: mpsc::UnboundedReceiver<RuntimeCommand>,
    llm: Arc<dyn crate::llm::provider::LlmProvider>,
    extensions: crate::ext::ExtensionManager,
    session: Arc<Mutex<crate::runtime::RuntimeSession>>,
    approval_mode: crate::tools::ApprovalMode,
    // In-process hand-off for `ReloadExtensions`. When a frontend shares the
    // Lua VM with the daemon (the in-process TUI), it boots the extensions
    // once and drops the cloned result here, letting the daemon adopt it
    // instead of re-reading disk and booting a second VM. `None` (e.g. `bone
    // serve`) falls back to booting from disk.
    reload_inbox: Option<Arc<Mutex<Option<crate::ext::BootedTools>>>>,
    // Forward Lua `ViewDiff`s (pane/UI updates) as `RuntimeEvent::ViewDiff` so a
    // *remote* frontend renders them. The in-process TUI shares the VM and
    // drains the `UiState` itself, so it passes `false` to avoid a double-drain
    // race; `bone serve` passes `true`.
    forward_view_diffs: bool,
    // Inject background sub-agent results / Lua-submitted prompts as turns from
    // the daemon when idle. `true` for `bone serve` (remote clients cannot
    // self-inject); `false` for the in-process TUI (it injects locally).
    inject_background: bool,
) {
    let mut ctx = DaemonCtx {
        hub: hub.into(),
        llm,
        extensions,
        session,
        mode: crate::tools::SharedApprovalMode::new(approval_mode),
        approval_registry: crate::runtime::ApprovalReplyRegistry::new(),
        key_registry: crate::runtime::KeyReplyRegistry::new(),
        reload_inbox,
        forward_view_diffs,
    };

    // Each command is serviced by `handle_idle_command`; the two that run a model
    // turn (`SubmitPrompt` / a submitting `RunCommand`) return `StartTurn(text)`,
    // which `run_turn` builds and pumps to completion. When `inject_background`
    // is set, an idle poll also drains background sub-agent results and
    // Lua-submitted prompts and runs them as turns — the daemon-side equivalent
    // of the TUI's `tick_jobs` / `tick_inbox`, so remote clients reach parity.
    let mut inject_timer = tokio::time::interval(std::time::Duration::from_millis(200));
    loop {
        let flow = tokio::select! {
            biased;
            cmd = commands.recv() => match cmd {
                Some(cmd) => ctx.handle_idle_command(cmd, &mut commands).await,
                None => break,
            },
            _ = inject_timer.tick(), if inject_background => {
                match ctx.next_background_prompt() {
                    // Route through the same `SubmitPrompt` handling as a typed
                    // prompt (transcript push, DB persist, `message` hook).
                    Some(text) => {
                        ctx.handle_idle_command(
                            RuntimeCommand::SubmitPrompt { text, images: vec![] },
                            &mut commands,
                        )
                        .await
                    }
                    None => Flow::Continue,
                }
            }
        };
        if let Flow::StartTurn(text) = flow {
            ctx.run_turn(text, &mut commands).await;
        }
    }
}

#[cfg(test)]
#[path = "rpc_tests.rs"]
mod rpc_tests;

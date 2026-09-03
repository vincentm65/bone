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

use futures_util::future::{AbortHandle, Abortable, FutureExt, LocalBoxFuture};
use futures_util::stream::{FuturesUnordered, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::llm::ChatMessage;
use crate::runtime::{RuntimeCommand, RuntimeEvent};

/// Fans [`RuntimeEvent`]s out to all attached clients and merges every client's
/// [`RuntimeCommand`]s into a single receiver the runtime consumes.
#[derive(Clone)]
pub struct Hub {
    events_tx: Arc<broadcast::Sender<RuntimeEvent>>,
    commands_tx: mpsc::UnboundedSender<RuntimeCommand>,
    group: Option<HubGroup>,
    busy: Arc<std::sync::atomic::AtomicBool>,
}

/// Event fan-out and host-control plane shared by all conversation actors in
/// one daemon. Conversation commands stay on each [`Hub`]; only explicitly
/// host-scoped operations use this group.
#[derive(Clone)]
pub struct HubGroup(Arc<HubGroupInner>);

struct HubGroupInner {
    events: Mutex<Vec<std::sync::Weak<broadcast::Sender<RuntimeEvent>>>>,
    extension_reloads: tokio::sync::watch::Sender<ExtensionReloadRequest>,
    extension_sources: Arc<Mutex<ExtensionSourceState>>,
    host: std::sync::OnceLock<crate::host::HostService>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ExtensionReloadRequest {
    authority_conversation_id: i64,
    skip_conversation_id: Option<i64>,
}

#[derive(Debug, Default)]
struct ExtensionSourceState {
    loaded: Option<crate::ext::source_stamp::SourceHash>,
    attempted: Option<crate::ext::source_stamp::SourceHash>,
    claiming: bool,
    scan_error: Option<String>,
}

impl Default for HubGroup {
    fn default() -> Self {
        let (extension_reloads, _) = tokio::sync::watch::channel(Default::default());
        Self(Arc::new(HubGroupInner {
            events: Mutex::new(Vec::new()),
            extension_reloads,
            extension_sources: Arc::new(Mutex::new(ExtensionSourceState::default())),
            host: std::sync::OnceLock::new(),
        }))
    }
}

impl HubGroup {
    fn request_extension_reload(
        &self,
        authority_conversation_id: i64,
        skip_conversation_id: Option<i64>,
    ) {
        self.0
            .extension_reloads
            .send_replace(ExtensionReloadRequest {
                authority_conversation_id,
                skip_conversation_id,
            });
    }

    fn subscribe_extension_reloads(&self) -> tokio::sync::watch::Receiver<ExtensionReloadRequest> {
        self.0.extension_reloads.subscribe()
    }

    fn host_service(&self, config: crate::config::store::ConfigStore) -> crate::host::HostService {
        self.0
            .host
            .get_or_init(|| crate::host::HostService::new(config))
            .clone()
    }
}

/// Runtime-side half of a [`Hub`]. It can publish events but deliberately does
/// not retain a command sender, so dropping every client closes the command
/// receiver and lets an in-process daemon terminate naturally.
#[derive(Clone)]
pub struct HubPublisher {
    events_tx: Arc<broadcast::Sender<RuntimeEvent>>,
    group: Option<HubGroup>,
    busy: Arc<std::sync::atomic::AtomicBool>,
}

struct TurnGuard(Arc<std::sync::atomic::AtomicBool>);

impl Drop for TurnGuard {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

impl HubPublisher {
    fn begin_turn(&self) -> TurnGuard {
        self.busy.store(true, std::sync::atomic::Ordering::SeqCst);
        TurnGuard(self.busy.clone())
    }

    /// Broadcast an event to every attached client.
    pub fn publish(&self, event: RuntimeEvent) {
        let _ = self.events_tx.send(event);
    }

    /// Broadcast daemon-global state to clients attached to every conversation.
    pub fn publish_global(&self, event: RuntimeEvent) {
        if let Some(group) = &self.group {
            group
                .0
                .events
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|sender| {
                    sender.upgrade().is_some_and(|sender| {
                        let _ = sender.send(event.clone());
                        true
                    })
                });
        } else {
            self.publish(event);
        }
    }
}

impl From<Hub> for HubPublisher {
    fn from(hub: Hub) -> Self {
        // Moving out `events_tx` drops `commands_tx` during conversion, so the
        // runtime cannot accidentally keep its own command receiver alive.
        Self {
            events_tx: hub.events_tx,
            group: hub.group,
            busy: hub.busy,
        }
    }
}

impl Hub {
    /// Create a hub and the single command receiver the runtime reads from.
    pub fn new() -> (Self, mpsc::UnboundedReceiver<RuntimeCommand>) {
        Self::new_inner(None)
    }

    pub fn new_grouped(group: HubGroup) -> (Self, mpsc::UnboundedReceiver<RuntimeCommand>) {
        Self::new_inner(Some(group))
    }

    fn new_inner(group: Option<HubGroup>) -> (Self, mpsc::UnboundedReceiver<RuntimeCommand>) {
        let (events_tx, _) = broadcast::channel(1024);
        let events_tx = Arc::new(events_tx);
        if let Some(group) = &group {
            group
                .0
                .events
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(Arc::downgrade(&events_tx));
        }
        let (commands_tx, commands_rx) = mpsc::unbounded_channel();
        (
            Self {
                events_tx,
                commands_tx,
                group,
                busy: Arc::new(std::sync::atomic::AtomicBool::new(false)),
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
            group: self.group.clone(),
            busy: self.busy.clone(),
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

    /// Whether this conversation actor currently has an active turn.
    pub fn is_busy(&self) -> bool {
        self.busy.load(std::sync::atomic::Ordering::SeqCst)
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
/// disconnects. The typed projection is evaluated for every attachment, so a
/// new client receives live actor state rather than caller-captured boot data.
pub struct ManagedRuntime {
    pub conversation_id: i64,
    pub hub: Hub,
    pub projection: RuntimeProjection,
    pub task: LocalBoxFuture<'static, ()>,
}

/// Shared live projection used by a managed actor's per-attachment replay.
///
/// The session remains authoritative; the mutable runtime slot follows provider
/// and extension replacement so late attachments never replay boot-time data.
#[derive(Clone)]
pub struct RuntimeProjection {
    session: Arc<Mutex<crate::runtime::RuntimeSession>>,
    runtime: Arc<Mutex<RuntimeProjectionState>>,
}

struct RuntimeProjectionState {
    llm: Arc<dyn crate::llm::provider::LlmProvider>,
    extensions: crate::ext::ExtensionManager,
}

impl RuntimeProjection {
    pub fn new(
        session: Arc<Mutex<crate::runtime::RuntimeSession>>,
        llm: Arc<dyn crate::llm::provider::LlmProvider>,
        extensions: crate::ext::ExtensionManager,
    ) -> Self {
        Self {
            session,
            runtime: Arc::new(Mutex::new(RuntimeProjectionState { llm, extensions })),
        }
    }

    /// Build the authoritative replay for one newly attached client.
    pub fn initial_events(&self, busy: bool) -> Vec<RuntimeEvent> {
        let (llm, extensions) = {
            let runtime = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
            (runtime.llm.clone(), runtime.extensions.clone())
        };
        let session = self.session.lock().unwrap_or_else(|e| e.into_inner());
        let snapshot = session.snapshot(llm.id(), llm.model());
        vec![
            frontend_state(&extensions, &session.tools),
            RuntimeEvent::StateSnapshot {
                snapshot: snapshot.clone(),
            },
            // Always send this, including for an empty new conversation, so
            // switching actors clears stale frontend scrollback.
            RuntimeEvent::ConversationLoaded {
                messages: session.display_transcript(),
                snapshot,
                busy,
            },
            // Apply the full view after ConversationLoaded resets transient
            // client state; otherwise the reset can immediately discard panes
            // from this authoritative projection.
            view_snapshot(&extensions),
        ]
    }

    fn replace_runtime(
        &self,
        llm: Arc<dyn crate::llm::provider::LlmProvider>,
        extensions: crate::ext::ExtensionManager,
    ) {
        let mut runtime = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
        runtime.llm = llm;
        runtime.extensions = extensions;
    }
}

struct ManagedEntry {
    hub: Hub,
    projection: RuntimeProjection,
    abort: AbortHandle,
    generation: u64,
    last_used: u64,
}

impl ManagedEntry {
    fn attach(&mut self, conversation_id: i64, clock: u64) -> SessionAttachment {
        self.last_used = clock;
        SessionAttachment {
            conversation_id,
            commands: self.hub.command_sender(),
            events: self.hub.subscribe(),
            initial: self.projection.initial_events(self.hub.is_busy()),
            group: self.hub.group.clone(),
        }
    }
}

const MAX_CACHED_ACTORS: usize = 16;

struct SessionAttachment {
    conversation_id: i64,
    commands: mpsc::UnboundedSender<RuntimeCommand>,
    events: broadcast::Receiver<RuntimeEvent>,
    initial: Vec<RuntimeEvent>,
    group: Option<HubGroup>,
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
    // Generation tags prevent a retired actor from removing a replacement that
    // was created for the same durable conversation before it finished exiting.
    let mut actors = FuturesUnordered::<LocalBoxFuture<'static, (i64, u64)>>::new();
    let mut latest_id = None;
    let mut generation = 0u64;
    let mut clock = 0u64;

    loop {
        tokio::select! {
            request = receiver.receiver.recv() => {
                let Some(SessionRequest::Attach { target, reply }) = request else {
                    break;
                };

                clock = clock.wrapping_add(1);
                let requested_id = match target {
                    SessionTarget::Conversation(id) => Some(id),
                    SessionTarget::Latest => latest_id,
                    SessionTarget::New => None,
                };
                if let Some(id) = requested_id
                    && let Some(entry) = sessions.get_mut(&id)
                {
                    let _ = reply.send(Ok(entry.attach(id, clock)));
                    latest_id = Some(id);
                    continue;
                }

                // Make room only when creating an actor. Attached actors are
                // never evicted; the least recently attached idle actor goes.
                while sessions.len() >= MAX_CACHED_ACTORS {
                    let Some(id) = sessions
                        .iter()
                        .filter(|(_, entry)| {
                            entry.hub.client_count() == 0 && !entry.hub.is_busy()
                        })
                        .min_by_key(|(_, entry)| entry.last_used)
                        .map(|(id, _)| *id)
                    else {
                        break;
                    };
                    if let Some(entry) = sessions.remove(&id) {
                        entry.abort.abort();
                    }
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
                            generation = generation.wrapping_add(1);
                            let actor_generation = generation;
                            let (abort, registration) = AbortHandle::new_pair();
                            let publisher = runtime.hub.publisher();
                            let task = runtime.task;
                            actors.push(Box::pin(async move {
                                let run = async move {
                                    if let Err(payload) = std::panic::AssertUnwindSafe(task)
                                        .catch_unwind()
                                        .await
                                    {
                                        publisher.publish(RuntimeEvent::Status {
                                            message: format!(
                                                "conversation runtime panicked: {}",
                                                crate::runtime::panic_message(payload.as_ref())
                                            ),
                                        });
                                    }
                                };
                                let _ = Abortable::new(run, registration).await;
                                (id, actor_generation)
                            }));
                            entry.insert(ManagedEntry {
                                hub: runtime.hub,
                                projection: runtime.projection,
                                abort,
                                generation: actor_generation,
                                last_used: clock,
                            });
                        }
                        latest_id = Some(id);
                        let entry = sessions.get_mut(&id).expect("managed session inserted");
                        let _ = reply.send(Ok(entry.attach(id, clock)));
                    }
                    Err(err) => { let _ = reply.send(Err(err)); }
                }
            }
            Some((id, generation)) = actors.next(), if !actors.is_empty() => {
                // Do not let a retired actor remove a replacement for the same
                // durable conversation.
                if sessions.get(&id).is_some_and(|entry| entry.generation == generation) {
                    sessions.remove(&id);
                }
            }
        }
    }
}

async fn attach_with_initial<W: AsyncWrite + Unpin>(
    manager: &SessionManager,
    target: SessionTarget,
    writer: &mut W,
) -> std::io::Result<Result<SessionAttachment, String>> {
    let mut attachment = match manager.attach(target).await {
        Ok(attachment) => attachment,
        Err(error) => return Ok(Err(error)),
    };
    for event in attachment.initial.drain(..) {
        codec::write_message(writer, &event).await?;
    }
    Ok(Ok(attachment))
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
    let mut attachment = attach_with_initial(&manager, initial_target, &mut write_half)
        .await?
        .map_err(std::io::Error::other)?;

    loop {
        tokio::select! {
            incoming = reader.read::<RuntimeCommand>() => match incoming {
                Some(Ok(RuntimeCommand::LoadConversation { id })) => {
                    match attach_with_initial(
                        &manager,
                        SessionTarget::Conversation(id),
                        &mut write_half,
                    ).await? {
                        Ok(next) => attachment = next,
                        Err(message) => codec::write_message(
                            &mut write_half,
                            &RuntimeEvent::ConversationLoadFailed { id, message },
                        ).await?,
                    }
                }
                Some(Ok(RuntimeCommand::NewConversation)) => {
                    match attach_with_initial(
                        &manager,
                        SessionTarget::New,
                        &mut write_half,
                    ).await? {
                        Ok(next) => attachment = next,
                        Err(message) => codec::write_message(
                            &mut write_half,
                            &RuntimeEvent::Status { message },
                        ).await?,
                    }
                }
                Some(Ok(RuntimeCommand::ReloadExtensions)) if attachment.group.is_some() => {
                    attachment
                        .group
                        .as_ref()
                        .expect("guarded above")
                        .request_extension_reload(attachment.conversation_id, None);
                }
                Some(Ok(command)) => {
                    if attachment.commands.send(command).is_err() {
                        codec::write_message(
                            &mut write_half,
                            &RuntimeEvent::Status { message: "conversation runtime stopped".into() },
                        ).await?;
                        return Ok(());
                    }
                }
                Some(Err(err)) => match err.into_fatal_io() {
                    Some(err) => return Err(err),
                    None => continue,
                },
                None => return Ok(()),
            },
            event = attachment.events.recv() => match event {
                Ok(event) => codec::write_message(&mut write_half, &event).await?,
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    codec::write_message(
                        &mut write_half,
                        &RuntimeEvent::StreamLagged { skipped },
                    )
                    .await?;
                }
                Err(broadcast::error::RecvError::Closed) => {
                    codec::write_message(
                        &mut write_half,
                        &RuntimeEvent::Status { message: "conversation runtime stopped".into() },
                    ).await?;
                    return Ok(());
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
    /// Cleared by the socket forwarder on EOF so all receivers observe
    /// `RecvError::Closed` even while the `RemoteClient` handle remains alive.
    events_tx: Arc<std::sync::Mutex<Option<broadcast::Sender<RuntimeEvent>>>>,
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
        let events_tx = Arc::new(std::sync::Mutex::new(Some(events_tx)));
        let forward_events = Arc::clone(&events_tx);
        let forwarder = tokio::spawn(async move {
            // `send` errors only when there are no receivers; that's fine — an
            // event with no subscriber is simply dropped, like the live Hub.
            while let Some(ev) = conn.next_event().await {
                let sender = forward_events.lock().unwrap().as_ref().cloned();
                let Some(sender) = sender else {
                    break;
                };
                let _ = sender.send(ev);
            }
            forward_events.lock().unwrap().take();
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
        } else if let Some(sender) = self.events_tx.lock().unwrap().as_ref() {
            sender.subscribe()
        } else {
            let (sender, receiver) = broadcast::channel(1);
            drop(sender);
            receiver
        }
    }
}

impl Drop for RemoteClient {
    fn drop(&mut self) {
        self.forwarder.abort();
        self.events_tx.lock().unwrap().take();
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
            .enabled_commands()
            .into_iter()
            .map(|c| (c.name, c.description))
            .collect(),
        tool_defs: tools.definitions(),
        tool_display: serde_json::to_value(tools.display_map()).unwrap_or_default(),
        subagents: extensions.subagents(),
        host_api_version: bone_protocol::HOST_API_VERSION,
        catalog_updates: crate::ext::catalog::updates_available(),
    }
}

/// Build the full canonical UI projection for attach and lag recovery.
pub fn view_snapshot(extensions: &crate::ext::ExtensionManager) -> RuntimeEvent {
    let view = crate::ext::api_ui::snapshot(&extensions.ui_handle());
    RuntimeEvent::ViewSnapshot { view: view.into() }
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
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    if codec::write_message(&mut w, &RuntimeEvent::StreamLagged { skipped })
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
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
            Err(err) => {
                let Some(err) = err.into_fatal_io() else {
                    continue;
                };
                writer.abort();
                return Err(err);
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
    StartTurn {
        /// Correlates a direct prompt or submitting command with completion.
        request_id: Option<u64>,
        text: String,
        display: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReloadReason {
    Manual,
    Automatic,
    Peer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum InteractionId {
    Approval(u64),
    Key(u64),
}

/// Interactive gates published to clients but not answered yet.
///
/// Cached request events let `Synchronize` replay a lost gate before its state
/// reply without duplicating reply-channel state outside the registries.
#[derive(Default)]
struct PendingInteractions {
    events: std::collections::BTreeMap<InteractionId, RuntimeEvent>,
}

impl PendingInteractions {
    fn track(&mut self, event: &RuntimeEvent) {
        let id = match event {
            RuntimeEvent::ApprovalRequest { id, .. } => InteractionId::Approval(*id),
            RuntimeEvent::KeyRequest { id } => InteractionId::Key(*id),
            _ => return,
        };
        self.events.insert(id, event.clone());
    }

    fn remove(&mut self, id: InteractionId) {
        self.events.remove(&id);
    }

    fn replay(&self, hub: &HubPublisher) {
        for event in self.events.values() {
            hub.publish(event.clone());
        }
    }

    fn clear(&mut self) {
        self.events.clear();
    }
}

fn is_config_command(command: &RuntimeCommand) -> bool {
    matches!(
        command,
        RuntimeCommand::GetConfig
            | RuntimeCommand::SetConfigValue { .. }
            | RuntimeCommand::ResetConfigValue { .. }
            | RuntimeCommand::UpsertProvider { .. }
            | RuntimeCommand::DeleteProvider { .. }
            | RuntimeCommand::SetActiveProvider { .. }
            | RuntimeCommand::SetToolEnabled { .. }
            | RuntimeCommand::SetCommandEnabled { .. }
            | RuntimeCommand::ReloadSettings
            | RuntimeCommand::UpsertSubagent { .. }
            | RuntimeCommand::DeleteSubagent { .. }
            | RuntimeCommand::SetSubagentEnabled { .. }
            | RuntimeCommand::HostRequest { .. }
    )
}

fn starts_turn(command: &RuntimeCommand) -> bool {
    matches!(
        command,
        RuntimeCommand::SubmitPrompt { .. } | RuntimeCommand::RunCommand { .. }
    )
}

fn checks_extension_sources(command: &RuntimeCommand) -> bool {
    match command {
        RuntimeCommand::SubmitPrompt { .. } => true,
        RuntimeCommand::RunCommand { name, input, .. } => {
            // The config command turns this into ReloadExtensions. Let that
            // authoritative manual request perform the one reload.
            name != "config" || !input.split_whitespace().eq(["tools", "reload"])
        }
        _ => false,
    }
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
    /// Steering prompts owned by this conversation actor's Lua runtime.
    submit_inbox: crate::ext::inbox::SubmitInbox,
    session: Arc<Mutex<crate::runtime::RuntimeSession>>,
    /// Immutable manager key for host-scoped routing. Unlike the session's
    /// persisted conversation id, this survives incognito transitions.
    actor_id: Option<i64>,
    mode: crate::tools::SharedApprovalMode,
    approval_registry: crate::runtime::ApprovalReplyRegistry,
    key_registry: crate::runtime::KeyReplyRegistry,
    pending_interactions: PendingInteractions,
    /// Commands received while a turn or interactive command owns the runtime.
    /// They are serviced in arrival order as soon as the runtime is idle.
    pending_commands: std::collections::VecDeque<RuntimeCommand>,
    /// Optional single-boot reload handoff from an in-process frontend.
    reload_inbox: Option<Arc<Mutex<Option<crate::ext::BootedTools>>>>,
    /// Source version shared by grouped actors, or local to this runtime.
    extension_sources: Arc<Mutex<ExtensionSourceState>>,
    /// Whether this actor forwards Lua view diffs to clients.
    forward_view_diffs: bool,
    /// Sole live configuration authority for this daemon runtime.
    config: crate::config::store::ConfigStore,
    /// Blocking daemon-global storage and setup authority.
    host: crate::host::HostService,
    /// Last process registry version published for the attached conversation.
    processes_seen: Option<(String, u64)>,
    /// Last job registry version published for the attached conversation.
    jobs_seen: Option<(i64, u64)>,
    /// Live runtime metadata read by managed late-attachment replays.
    projection: Option<RuntimeProjection>,
    /// Persistent frontend event stream. The turn's `Driver` (and approval
    /// gate) emit here, so events outlive the per-turn `LocalConn` channel:
    /// off-VM work that finishes after the turn — e.g. an idle `ctx.time.after`
    /// recap timer — still reaches attached clients. The turn pump drains it
    /// while a turn runs; the daemon loop's select drains it while idle. The
    /// sender kept here is a keep-alive, so the receiver never disconnects.
    background_events_tx: mpsc::UnboundedSender<RuntimeEvent>,
    background_events_rx: mpsc::UnboundedReceiver<RuntimeEvent>,
}

fn job_snapshot(job: crate::ext::jobs::Job) -> Option<bone_protocol::JobSnapshot> {
    if job.cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
        return None;
    }
    let status = match job.status {
        crate::ext::jobs::JobStatus::Queued => bone_protocol::JobStatus::Queued,
        crate::ext::jobs::JobStatus::Running => bone_protocol::JobStatus::Running,
        crate::ext::jobs::JobStatus::Done | crate::ext::jobs::JobStatus::Error => return None,
    };
    let events = job
        .events
        .into_iter()
        .filter_map(|entry| match entry.event {
            RuntimeEvent::TextDelta { text } => {
                Some(bone_protocol::JobEventSnapshot::TextDelta { text })
            }
            RuntimeEvent::ReasoningDelta { text } => {
                Some(bone_protocol::JobEventSnapshot::ReasoningDelta { text })
            }
            RuntimeEvent::ToolCall {
                id,
                name,
                arguments,
                ..
            } => Some(bone_protocol::JobEventSnapshot::ToolCall {
                id,
                name,
                arguments,
                edit_preview: entry.edit_preview,
            }),
            RuntimeEvent::ToolResult {
                name,
                call_id,
                is_error,
                content,
            } => Some(bone_protocol::JobEventSnapshot::ToolResult {
                name,
                call_id,
                is_error,
                content,
            }),
            RuntimeEvent::Failed { message } => {
                Some(bone_protocol::JobEventSnapshot::Failed { message })
            }
            _ => None,
        })
        .collect();
    Some(bone_protocol::JobSnapshot {
        id: job.id,
        agent: job.agent,
        task: job.task,
        title: job.title,
        status,
        started_at: job.started_at,
        token_sent: job.token_sent,
        token_received: job.token_received,
        provider: job.provider,
        activity: job.activity,
        events,
    })
}

fn bounded_jobs_snapshot(
    version: u64,
    mut jobs: Vec<bone_protocol::JobSnapshot>,
    max_bytes: usize,
) -> RuntimeEvent {
    let encoded_len = |jobs: &Vec<bone_protocol::JobSnapshot>| {
        serde_json::to_vec(&RuntimeEvent::JobsSnapshot {
            version,
            jobs: jobs.clone(),
        })
        .expect("job snapshots are serializable")
        .len()
    };
    let mut total = encoded_len(&jobs);

    for job in &mut jobs {
        let mut remove_count = 0;
        while total > max_bytes && remove_count < job.events.len() {
            let event_len = serde_json::to_vec(&job.events[remove_count])
                .expect("job events are serializable")
                .len();
            let comma_len = usize::from(job.events.len() - remove_count > 1);
            total = total.saturating_sub(event_len + comma_len);
            remove_count += 1;
        }
        job.events.drain(..remove_count);
        if total <= max_bytes {
            break;
        }
    }

    while total > max_bytes && !jobs.is_empty() {
        jobs.pop();
        total = encoded_len(&jobs);
    }

    RuntimeEvent::JobsSnapshot { version, jobs }
}

struct BlockingCtxSetup {
    app_state: crate::ext::ctx::AppCtxState,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    usage_records: Arc<Mutex<Vec<crate::ext::ctx::PrivateLlmUsage>>>,
    provider_id: String,
    provider_model: String,
    provider: Arc<dyn crate::llm::provider::LlmProvider>,
    ui: crate::ext::api_ui::SharedUi,
    key_tx: Option<mpsc::UnboundedSender<crate::pane_content::KeyRequest>>,
    status_tx: Option<mpsc::UnboundedSender<RuntimeEvent>>,
    key_rx: mpsc::UnboundedReceiver<crate::pane_content::KeyRequest>,
    status_rx: mpsc::UnboundedReceiver<RuntimeEvent>,
    approval_gate: crate::tools::SharedGate,
}

impl BlockingCtxSetup {
    fn new(daemon: &DaemonCtx) -> Self {
        let (key_tx, key_rx) = mpsc::unbounded_channel();
        let (status_tx, status_rx) = mpsc::unbounded_channel();
        let app_state = daemon.app_ctx_snapshot();
        let approval_gate =
            crate::tools::SharedGate(Arc::new(crate::runtime::ChannelApprovalGate::new(
                status_tx.clone(),
                daemon.approval_registry.clone(),
                None,
                app_state.tool_handler.working_dir.clone(),
            )));
        Self {
            app_state,
            cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            usage_records: Arc::new(Mutex::new(Vec::new())),
            provider_id: daemon.llm.id().to_string(),
            provider_model: daemon.llm.model().to_string(),
            provider: Arc::clone(&daemon.llm),
            ui: daemon.extensions.ui_handle(),
            key_tx: Some(key_tx),
            status_tx: Some(status_tx),
            key_rx,
            status_rx,
            approval_gate,
        }
    }

    fn ctx_config(
        &mut self,
        conversation_tx: Option<std::sync::mpsc::Sender<crate::ext::ctx::ConversationOperation>>,
    ) -> crate::ext::ctx::CtxConfig {
        let mut config = crate::ext::ctx::CtxConfig::new(
            crate::config::bone_dir().to_string_lossy().to_string(),
            self.app_state.tool_handler.shared_state.clone(),
            self.app_state.config_store.clone(),
            self.app_state.config_schema.clone(),
        );
        self.app_state.apply_to(&mut config);
        config.key_sender = self.key_tx.take();
        config.runtime_status = self.status_tx.take();
        config.approval_gate = Some(self.approval_gate.clone());
        if let Some(handler) = config.tool_handler.as_mut() {
            handler.approval_gate = Some(self.approval_gate.clone());
        }
        config.ui = Some(self.ui.clone());
        config.cancelled = Some(self.cancel.clone());
        config.private_llm = Some(crate::ext::ctx::PrivateLlmContext {
            provider: Arc::clone(&self.provider),
            request_context: crate::llm::provider::ProviderRequestContext {
                conversation_id: self.app_state.session_id,
                cache_scope: Some(crate::llm::provider::new_cache_scope(
                    self.app_state.session_id,
                    self.app_state.background_scope,
                )),
                turn_state: Some(Arc::new(std::sync::OnceLock::new())),
                max_tokens: None,
            },
            usage_records: Arc::clone(&self.usage_records),
        });
        config.conversation_operations = conversation_tx;
        config
    }
}

enum BlockingPumpResult<T> {
    Finished(Option<T>),
    Shutdown(Option<T>),
}

impl DaemonCtx {
    fn publish_runtime_event(&mut self, event: RuntimeEvent) {
        self.pending_interactions.track(&event);
        self.hub.publish(event);
    }

    fn record_private_llm_usage(
        &mut self,
        records: &Arc<Mutex<Vec<crate::ext::ctx::PrivateLlmUsage>>>,
        provider: &str,
        model: &str,
    ) {
        let records = std::mem::take(
            &mut *records
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        );
        for usage in records {
            let (sent, received, context_length, persistence_error) = {
                let mut session = self.session.lock().unwrap();
                session.token_stats.record_request(
                    usage.prompt_tokens,
                    usage.completion_tokens,
                    usage.cached_tokens,
                    usage.cost,
                );
                let persistence_error = match (session.session_db.as_ref(), session.conversation_id)
                {
                    (Some(db), Some(conversation_id)) => db
                        .record_usage(
                            conversation_id,
                            provider,
                            model,
                            usage.prompt_tokens,
                            usage.completion_tokens,
                            usage.cached_tokens,
                            usage.cost,
                            usage.is_estimated,
                        )
                        .err()
                        .map(|error| error.to_string()),
                    _ => None,
                };
                (
                    session.token_stats.sent,
                    session.token_stats.received,
                    session.token_stats.context_length,
                    persistence_error,
                )
            };
            self.publish_runtime_event(RuntimeEvent::TokenUsage {
                sent,
                received,
                context_length,
            });
            if let Some(error) = persistence_error {
                self.hub.publish(RuntimeEvent::Status {
                    message: format!("failed to persist command LLM usage: {error}"),
                });
            }
        }
    }

    fn config_schema(&self) -> bone_protocol::ConfigSchema {
        let tools = self
            .session
            .lock()
            .unwrap()
            .tools
            .all_definitions()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();
        let commands = self
            .extensions
            .commands()
            .iter()
            .map(|command| command.name.clone())
            .collect::<Vec<_>>();
        self.config.schema_for(&tools, &commands)
    }

    fn config_event(&self) -> RuntimeEvent {
        RuntimeEvent::ConfigSnapshot {
            schema: self.config_schema(),
            snapshot: self.config.snapshot(),
        }
    }

    fn publish_config(&self) {
        self.hub.publish_global(self.config_event());
    }

    fn finish_config_mutation(
        &self,
        changed_paths: Vec<String>,
        result: Result<(), (u64, String)>,
        restart_required: bool,
        request_id: Option<String>,
    ) {
        match result {
            Ok(()) => {
                self.hub.publish_global(RuntimeEvent::ConfigChanged {
                    changed_paths,
                    schema: self.config_schema(),
                    snapshot: self.config.snapshot(),
                    restart_required,
                    request_id,
                });
                self.hub.publish_global(frontend_state(
                    &self.extensions,
                    &self.session.lock().unwrap().tools,
                ));
            }
            Err((current_revision, error)) => {
                self.hub.publish(RuntimeEvent::ConfigMutationRejected {
                    current_revision,
                    error,
                    request_id,
                })
            }
        }
    }

    /// Publish a `StateSnapshot` derived from the current session + provider.
    /// Swap the active provider (and model) to the given ids, e.g. when loading
    /// a conversation that was created with a different provider. A no-op when
    /// already matching. Failure keeps the current provider — the caller still
    /// snapshots so the frontend proceeds with the old provider label.
    fn restore_provider(&mut self, provider_id: &str, model: &str) {
        if self.llm.id() == provider_id && self.llm.model() == model {
            return;
        }
        let providers_config = self.config.providers_config();
        match crate::llm::providers::build_provider(provider_id, model, &providers_config) {
            Ok(new_provider) => {
                self.llm = Arc::from(new_provider);
                self.refresh_projection();
            }
            Err(err) => self.hub.publish(RuntimeEvent::Status {
                message: format!("failed to restore provider `{provider_id}`: {err}"),
            }),
        }
    }

    fn publish_snapshot(&self) {
        self.refresh_projection();
        self.hub.publish(RuntimeEvent::StateSnapshot {
            snapshot: {
                let s = self.session.lock().unwrap();
                s.snapshot(self.llm.id(), self.llm.model())
            },
        });
    }

    fn publish_synchronized_state(&self, request_id: u64, include_messages: bool, busy: bool) {
        let (snapshot, messages) = {
            let session = self.session.lock().unwrap();
            let snapshot = session.snapshot(self.llm.id(), self.llm.model());
            let messages = include_messages.then(|| session.display_transcript());
            (snapshot, messages)
        };
        self.hub.publish(RuntimeEvent::StateSynchronized {
            request_id,
            busy,
            snapshot,
            view: Some(crate::ext::api_ui::snapshot(&self.extensions.ui_handle()).into()),
            messages,
        });
        // The correlated completion atomically replaces stale session + view
        // state. Replay live gates afterwards so applying that full view cannot
        // immediately erase a recovered approval/key prompt.
        self.pending_interactions.replay(&self.hub);
    }

    fn refresh_projection(&self) {
        if let Some(projection) = &self.projection {
            projection.replace_runtime(self.llm.clone(), self.extensions.clone());
        }
    }

    fn publish_processes(&mut self, force: bool) {
        let scope = crate::processes::conversation_scope(Some(
            self.session.lock().unwrap().background_scope(),
        ));
        let registry = crate::processes::registry();
        let version = registry.version();
        if !force && self.processes_seen.as_ref() == Some(&(scope.clone(), version)) {
            return;
        }
        let processes = registry
            .list(Some(&scope))
            .into_iter()
            .map(|process| bone_protocol::ProcessSnapshot {
                id: process.id,
                command: process.command,
                owner: process.owner,
                running: process.running,
                state: match process.state {
                    crate::processes::ProcessState::Running => bone_protocol::ProcessState::Running,
                    crate::processes::ProcessState::Exited => bone_protocol::ProcessState::Exited,
                    crate::processes::ProcessState::TimedOut => {
                        bone_protocol::ProcessState::TimedOut
                    }
                    crate::processes::ProcessState::Cancelled => {
                        bone_protocol::ProcessState::Cancelled
                    }
                },
                started_at: process.started_at,
                finished_at: process.finished_at,
                stdout: process.stdout,
                stderr: process.stderr,
                exit_code: process.exit_code,
                signal: process.signal,
                error: process.error,
            })
            .collect();
        self.processes_seen = Some((scope, version));
        self.hub
            .publish(RuntimeEvent::ProcessesSnapshot { version, processes });
    }

    fn publish_jobs(&mut self, force: bool) {
        let scope = self.session.lock().unwrap().background_scope();
        let registry = crate::ext::jobs::registry();
        let version = registry.version();
        if !force && self.jobs_seen == Some((scope, version)) {
            return;
        }
        let jobs = registry
            .running_jobs_scoped(Some(scope))
            .into_iter()
            .filter_map(job_snapshot)
            .collect();
        self.jobs_seen = Some((scope, version));
        self.hub.publish(bounded_jobs_snapshot(
            version,
            jobs,
            crate::rpc::codec::MAX_LINE_BYTES,
        ));
    }

    fn cancel_process(&mut self, id: &str) {
        let scope = crate::processes::conversation_scope(Some(
            self.session.lock().unwrap().background_scope(),
        ));
        crate::processes::registry().kill_scoped(&scope, id);
        self.publish_processes(true);
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

    /// Toggle session-scoped incognito mode. Publishes a Status (success or
    /// failure) and always a fresh snapshot so every client's view-model
    /// (including the INC badge) follows the daemon's authoritative state.
    fn set_incognito(&mut self, enabled: bool) {
        let changing = self.session.lock().unwrap().incognito != enabled;
        if changing {
            // Background work belongs to the scope being left. Cancel it before
            // `conversation_id` changes so its result cannot be orphaned.
            self.cancel_background_work();
        }
        let result = self
            .session
            .lock()
            .unwrap()
            .set_incognito(enabled, self.llm.as_ref());
        self.hub.publish(RuntimeEvent::Status {
            message: match result {
                Ok(()) if enabled => "Incognito on — chats are not saved".into(),
                Ok(()) => "Incognito off — saving resumed".into(),
                Err(err) => err,
            },
        });
        self.publish_snapshot();
    }

    fn finish_subagent_change(
        &self,
        path: String,
        result: Result<(), (u64, String)>,
        request_id: Option<String>,
    ) {
        self.finish_config_mutation(vec![path], result, false, request_id);
    }

    fn persist_mode(&self, mode_str: &str) {
        let mode = match crate::tools::ApprovalMode::parse(mode_str) {
            Ok(mode) => mode,
            Err(error) => {
                self.hub.publish(RuntimeEvent::Status { message: error });
                return;
            }
        };
        let revision = self.config.snapshot().revision;
        let result =
            self.config
                .set_value("general.approval", serde_json::json!(mode_str), revision);
        if result.is_ok() {
            self.mode.set(mode);
        }
        self.finish_config_mutation(vec!["general.approval".into()], result, false, None);
    }

    /// Reload canonical settings through the aggregate store so revisioned
    /// clients receive the same authoritative state as runtime consumers.
    fn reload_settings(&self) {
        let result = self.config.reload_settings().map_err(|error| {
            let revision = self.config.snapshot().revision;
            (revision, format!("settings reload failed: {error}"))
        });
        if result.is_ok()
            && let Some(approval) = self
                .config
                .snapshot()
                .values
                .pointer("/general/approval")
                .and_then(serde_json::Value::as_str)
        {
            self.set_mode(approval);
        }
        self.finish_config_mutation(vec!["config.yaml".into()], result, false, None);
    }

    /// Sync terminal width from the frontend so Lua panes wrap correctly.
    fn set_width(&self, width: u16) {
        let ui_handle = self.extensions.ui_handle();
        let mut ui = ui_handle.lock().unwrap_or_else(|e| e.into_inner());
        ui.terminal_width = width;
    }

    fn finalize_session_end(&self) {
        let session = self.session.lock().unwrap();
        if let (Some(db), Some(id)) = (session.session_db.as_ref(), session.conversation_id)
            && let Err(err) = db.end_conversation(id)
        {
            self.hub.publish(RuntimeEvent::Status {
                message: format!("failed to end conversation: {err}"),
            });
        }
    }

    fn app_ctx_snapshot(&self) -> crate::ext::ctx::AppCtxState {
        let config_schema = self.config_schema();
        let settings = self.config.runtime_settings_snapshot();
        let system_prompt =
            crate::llm::prompts::system_prompt(settings.resolved().general.system_prompt());
        let s = self.session.lock().unwrap();
        let by_provider =
            crate::ext::ctx::usage_by_provider_context(s.session_db.as_ref(), s.conversation_id);
        let mut state = crate::ext::ctx::AppCtxState::new(
            &s.tools,
            &s.token_stats,
            &self.mode.get(),
            s.conversation_id,
            self.llm.id(),
            self.llm.model(),
            self.llm.context_window_tokens(),
            Some(system_prompt),
            by_provider,
            s.transcript.clone(),
            self.config.clone(),
            config_schema,
        );
        state.background_scope = Some(s.background_scope());
        state
    }

    /// Pump daemon-owned blocking work while preserving the runtime control plane
    /// shared by managed hooks and interactive commands.
    async fn pump_blocking<T>(
        &mut self,
        commands: &mut mpsc::UnboundedReceiver<RuntimeCommand>,
        mut handle: tokio::task::JoinHandle<T>,
        setup: BlockingCtxSetup,
        cancel_background_on_disconnect: bool,
    ) -> BlockingPumpResult<T> {
        use std::sync::atomic::Ordering;

        let BlockingCtxSetup {
            cancel,
            usage_records: private_usage_records,
            provider_id: private_provider_id,
            provider_model: private_provider_model,
            key_rx: mut live_rx,
            mut status_rx,
            ..
        } = setup;
        let mut diff_timer = tokio::time::interval(std::time::Duration::from_millis(50));
        loop {
            tokio::select! {
                result = &mut handle => {
                    self.drain_diffs();
                    while let Ok(event) = status_rx.try_recv() {
                        self.publish_runtime_event(event);
                    }
                    self.record_private_llm_usage(
                        &private_usage_records,
                        &private_provider_id,
                        &private_provider_model,
                    );
                    self.pending_interactions.clear();
                    return BlockingPumpResult::Finished(result.ok());
                }
                Some(event) = status_rx.recv() => self.publish_runtime_event(event),
                Some(request) = live_rx.recv() => {
                    let id = self.key_registry.register(request);
                    self.publish_runtime_event(RuntimeEvent::KeyRequest { id });
                }
                _ = diff_timer.tick() => {
                    self.publish_processes(false);
                    self.publish_jobs(false);
                    self.drain_diffs();
                }
                command = commands.recv() => match command {
                    Some(RuntimeCommand::ApprovalReply { id, outcome }) => {
                        self.approval_registry.resolve(id, outcome);
                        self.pending_interactions.remove(InteractionId::Approval(id));
                    }
                    Some(RuntimeCommand::KeyReply { id, key }) => {
                        self.key_registry.resolve(id, key);
                        self.pending_interactions.remove(InteractionId::Key(id));
                    }
                    Some(RuntimeCommand::Synchronize { request_id, include_messages }) => {
                        self.publish_synchronized_state(request_id, include_messages, true)
                    }
                    Some(command) if is_config_command(&command) => {
                        let _ = Box::pin(self.handle_idle_command(command, commands)).await;
                    }
                    Some(RuntimeCommand::GetProcesses) => self.publish_processes(true),
                    Some(RuntimeCommand::GetJobs) => self.publish_jobs(true),
                    Some(RuntimeCommand::CancelProcess { id }) => self.cancel_process(&id),
                    Some(RuntimeCommand::Cancel) => {
                        self.cancel_background_work();
                        cancel.store(true, Ordering::Relaxed);
                        self.approval_registry.cancel_all();
                        self.key_registry.cancel_all();
                        let result = tokio::time::timeout(
                            std::time::Duration::from_millis(100),
                            &mut handle,
                        ).await.ok().and_then(Result::ok);
                        self.record_private_llm_usage(
                            &private_usage_records,
                            &private_provider_id,
                            &private_provider_model,
                        );
                        self.pending_interactions.clear();
                        self.drain_diffs();
                        return BlockingPumpResult::Shutdown(result);
                    }
                    None => {
                        if cancel_background_on_disconnect {
                            self.cancel_background_work();
                        }
                        cancel.store(true, Ordering::Relaxed);
                        self.approval_registry.cancel_all();
                        self.key_registry.cancel_all();
                        let result = tokio::time::timeout(
                            std::time::Duration::from_millis(100),
                            &mut handle,
                        ).await.ok().and_then(Result::ok);
                        self.record_private_llm_usage(
                            &private_usage_records,
                            &private_provider_id,
                            &private_provider_model,
                        );
                        self.pending_interactions.clear();
                        self.drain_diffs();
                        return BlockingPumpResult::Shutdown(result);
                    }
                    Some(command) => self.pending_commands.push_back(command),
                }
            }
        }
    }

    /// Run a lifecycle hook as daemon-owned work. The callback receives the same
    /// bounded context as an interactive Lua command, while the actor continues
    /// pumping approvals, key replies, UI diffs, jobs, processes, and cancellation.
    async fn run_managed_hook(
        &mut self,
        commands: &mut mpsc::UnboundedReceiver<RuntimeCommand>,
        name: String,
        payload: serde_json::Value,
        blockable: bool,
    ) -> crate::ext::types::ManagedHookResult {
        let mut setup = BlockingCtxSetup::new(self);
        let ctx_cfg = setup.ctx_config(None);
        let extensions = self.extensions.clone();
        let handle = tokio::task::spawn_blocking(move || {
            extensions.dispatch_managed(&name, payload, ctx_cfg, blockable)
        });
        match self.pump_blocking(commands, handle, setup, true).await {
            BlockingPumpResult::Finished(result) | BlockingPumpResult::Shutdown(result) => {
                result.unwrap_or_default()
            }
        }
    }

    /// Terminate every running background sub-agent and managed shell process
    /// for this session, surfacing notices when anything was cancelled. Called
    /// on turn cancel (Ctrl+C) and on conversation reset (`/new`, `/clear`).
    fn cancel_background_work(&mut self) {
        // Scope to this session's conversation so a process hosting several
        // conversations (`bone serve`) doesn't kill another one's work.
        let scope = self.session.lock().unwrap().background_scope();
        let cancelled_jobs = crate::ext::jobs::registry().cancel_all_scoped(Some(scope));
        if cancelled_jobs > 0 {
            self.hub.publish(RuntimeEvent::Status {
                message: format!("cancelled {cancelled_jobs} background sub-agent job(s)"),
            });
            self.publish_jobs(true);
        }

        let process_scope = crate::processes::conversation_scope(Some(scope));
        let cancelled_processes = crate::processes::registry().kill_all_scoped(&process_scope);
        if cancelled_processes > 0 {
            self.hub.publish(RuntimeEvent::Status {
                message: format!("cancelled {cancelled_processes} background shell process(es)"),
            });
        }
    }

    fn cancel_job(&mut self, id: &str) {
        let scope = self.session.lock().unwrap().background_scope();
        crate::ext::jobs::registry().cancel_scoped(id, Some(scope));
        self.publish_jobs(true);
    }

    /// Next queued background prompt to inject as a turn when the daemon is idle,
    /// or `None` when nothing is pending. Lua-submitted prompts (`bone.submit`)
    /// go first, one per idle tick; otherwise a batch of this conversation's
    /// finished sub-agent jobs is formatted into a single turn and marked
    /// consumed so it is never injected twice.
    fn next_background_prompt(&self) -> Option<(String, Option<String>)> {
        // `bone.submit` prompts first — steering should win over passively
        // arriving job results.
        if let Some(prompt) = self.submit_inbox.pop() {
            return Some((prompt.text, prompt.display));
        }
        let scope = self.session.lock().unwrap().background_scope();
        let registry = crate::ext::jobs::registry();
        let finished = registry.peek_finished_unconsumed_scoped(Some(scope));
        if finished.is_empty() {
            return None;
        }
        let running = registry.running_jobs_scoped(Some(scope));
        let (turn_text, display) =
            crate::ext::jobs::format_results_for_injection(&finished, &running)?;
        let ids: Vec<String> = finished.iter().map(|j| j.id.clone()).collect();
        registry.mark_consumed(&ids);
        Some((turn_text, Some(display)))
    }

    /// Run a registered Lua slash command inside the daemon, forwarding its pane
    /// diffs (`ViewDiff`) and interactive gates (`KeyRequest`,
    /// `ApprovalRequest`) to clients and pumping their replies or `Cancel`
    /// back, exactly like a turn.
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
    /// Daemon-side slash-command runner: lets a frontend run interactive
    /// commands against the daemon's Lua VM. The pure-client TUI routes all
    /// slash commands over this path.
    async fn run_interactive_command(
        &mut self,
        commands: &mut mpsc::UnboundedReceiver<RuntimeCommand>,
        name: String,
        input: String,
    ) -> Option<(
        Option<crate::ext::types::LuaCommandReturn>,
        Vec<crate::ext::ctx::ConversationOperation>,
    )> {
        if !self.extensions.command_enabled(&name) {
            return None;
        }

        let mut setup = BlockingCtxSetup::new(self);
        let (conversation_tx, conversation_rx) = std::sync::mpsc::channel();
        let ctx_cfg = setup.ctx_config(Some(conversation_tx));
        let lua = self.extensions.lua_handle();

        // The handler call blocks (Lua + nested tool calls), so run it off the
        // async runtime (spawn_blocking — the handler may nest tool calls).
        // Outer `Option` = "was the command found?"; inner `Option` = the parsed
        // result (a found handler may legitimately return a no-op `None`).
        let handle = tokio::task::spawn_blocking(move || {
            let lua_guard = lua.lock().unwrap_or_else(|e| e.into_inner());
            // Not found: the only case that should surface as "unknown command".
            let handler = crate::ext::ops_commands::find_handler(&lua_guard, &name)?;
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
        match self.pump_blocking(commands, handle, setup, false).await {
            BlockingPumpResult::Finished(result) => result.flatten(),
            BlockingPumpResult::Shutdown(_) => Some((None, Vec::new())),
        }
    }

    fn apply_conversation_operations(
        &mut self,
        operations: Vec<crate::ext::ctx::ConversationOperation>,
    ) {
        for operation in operations {
            match operation {
                crate::ext::ctx::ConversationOperation::Load(id) => self.load_conversation(id),
                crate::ext::ctx::ConversationOperation::Append(messages) => {
                    let settings = self.config.runtime_settings_snapshot();
                    let system_prompt = crate::llm::prompts::system_prompt(
                        settings.resolved().general.system_prompt(),
                    );
                    let mut session = self.session.lock().unwrap();
                    for mut message in messages {
                        if message.created_at.is_none() {
                            message.created_at = Some(crate::util::utc_now());
                        }
                        let role = message.role.as_str().to_string();
                        let tool_calls = serde_json::to_string(&message.tool_calls).ok();
                        let images = serde_json::to_string(&message.images).ok();
                        session.transcript.push(message.clone());
                        session.append_db_message(
                            &role,
                            &message.content,
                            message.name.as_deref(),
                            message.tool_call_id.as_deref(),
                            tool_calls.as_deref(),
                            images.as_deref(),
                        );
                    }
                    session.recompute_context_estimate(&system_prompt);
                }
            }
        }
    }

    fn load_conversation(&mut self, id: i64) {
        // Refuse while incognito: re-attaching `conversation_id` would silently
        // resume DB writes behind the INC badge. The user must explicitly turn
        // incognito off (which persists the pending transcript) first.
        if self.session.lock().unwrap().incognito {
            self.hub.publish(RuntimeEvent::ConversationLoadFailed {
                id,
                message: "cannot load a conversation while incognito — turn incognito off first"
                    .into(),
            });
            return;
        }
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
            let changing_conversation = self.session.lock().unwrap().conversation_id != Some(id);
            if changing_conversation {
                self.cancel_background_work();
            }
            if let Some((provider_id, model)) = provider_model {
                self.restore_provider(&provider_id, &model);
            }
            let messages = rows
                .into_iter()
                .map(crate::session_db::stored_to_chat_message)
                .collect::<Vec<_>>();
            let system_prompt = crate::llm::prompts::system_prompt(
                self.config
                    .runtime_settings_snapshot()
                    .resolved()
                    .general
                    .system_prompt(),
            );
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
                s.restore_usage_and_context(&system_prompt);
                s.snapshot(self.llm.id(), self.llm.model())
            };
            self.reset_host_tool_state();
            self.hub.publish(RuntimeEvent::ConversationLoaded {
                messages,
                snapshot,
                busy: false,
            });
            // ConversationLoaded resets client-owned transient UI state. Reapply
            // the canonical extension view afterwards so surviving status lines,
            // highlights, and non-conversation panes remain visible.
            self.hub.publish(view_snapshot(&self.extensions));
        } else {
            self.hub.publish(RuntimeEvent::ConversationLoadFailed {
                id,
                message: format!("failed to load conversation {id}"),
            });
        }
    }

    fn source_stamp(&self) -> Result<crate::ext::source_stamp::SourceHash, String> {
        crate::ext::source_stamp::stamp(&crate::config::bone_dir())
            .map_err(|error| error.to_string())
    }

    fn report_source_scan_error(&self, error: String) {
        let changed = {
            let mut state = self
                .extension_sources
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.scan_error.as_ref() == Some(&error) {
                false
            } else {
                state.scan_error = Some(error.clone());
                true
            }
        };
        if changed {
            self.hub.publish(RuntimeEvent::Status {
                message: format!("Could not check Lua extension changes: {error}"),
            });
        }
    }

    fn initialize_current_sources(&self) {
        match self.source_stamp() {
            Ok(current) => {
                let mut state = self
                    .extension_sources
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if state.loaded.is_none() {
                    state.loaded = Some(current);
                }
                state.scan_error = None;
            }
            Err(error) => self.report_source_scan_error(error),
        }
    }

    fn record_sources_loaded_if_unchanged(
        &self,
        expected: Option<crate::ext::source_stamp::SourceHash>,
    ) {
        let Some(expected) = expected else {
            return;
        };
        match self.source_stamp() {
            Ok(current) if current == expected => {
                let mut state = self
                    .extension_sources
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                state.loaded = Some(current);
                state.attempted = None;
                state.claiming = false;
                state.scan_error = None;
            }
            Ok(_) => {}
            Err(error) => self.report_source_scan_error(error),
        }
    }

    fn maybe_reload_changed_extensions(&mut self) {
        let current = match self.source_stamp() {
            Ok(current) => current,
            Err(error) => {
                self.report_source_scan_error(error);
                return;
            }
        };
        let claimed = {
            let mut state = self
                .extension_sources
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.scan_error = None;
            if state.loaded.is_none() {
                state.loaded = Some(current);
                false
            } else if state.loaded == Some(current)
                || state.attempted == Some(current)
                || state.claiming
            {
                false
            } else {
                state.attempted = Some(current);
                state.claiming = true;
                true
            }
        };
        if !claimed {
            return;
        }
        if !self.reload_extensions(true, ReloadReason::Automatic) {
            self.extension_sources
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .claiming = false;
            return;
        }

        let unchanged = match self.source_stamp() {
            Ok(after) if after == current => {
                let mut state = self
                    .extension_sources
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                state.loaded = Some(current);
                state.attempted = None;
                state.claiming = false;
                state.scan_error = None;
                true
            }
            Ok(_) => {
                self.extension_sources
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .claiming = false;
                false
            }
            Err(error) => {
                self.extension_sources
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .claiming = false;
                self.report_source_scan_error(error);
                false
            }
        };
        if unchanged
            && let Some(group) = &self.hub.group
            && let Some(actor_id) = self.actor_id
        {
            group.request_extension_reload(actor_id, Some(actor_id));
        }
    }

    /// Rebuild this actor's Lua/tool runtime while preserving its
    /// conversation-scoped host state. `catalog_authority` is true for the one
    /// actor selected by a host-scoped reload (or for an ungrouped in-process
    /// runtime); peers reload their isolated VM without racing to redefine the
    /// daemon's schema authority.
    fn reload_extensions(&mut self, catalog_authority: bool, reason: ReloadReason) -> bool {
        let expected_manual_source = if reason == ReloadReason::Manual {
            match self.source_stamp() {
                Ok(source) => Some(source),
                Err(error) => {
                    self.report_source_scan_error(error);
                    None
                }
            }
        } else {
            None
        };
        // The in-process handoff is prepared by the frontend for an explicit
        // reload. Automatic and peer reloads must boot the just-fingerprinted
        // disk tree instead of consuming a possibly older handoff.
        let handed_off = (reason == ReloadReason::Manual)
            .then(|| {
                self.reload_inbox
                    .as_ref()
                    .and_then(|inbox| inbox.lock().unwrap().take())
            })
            .flatten();
        let mut booted = match handed_off {
            Some(booted) => booted,
            None => {
                let config_dir = crate::config::bone_dir();
                let cwd = std::env::current_dir().unwrap_or_default();
                let model = self.llm.model().to_string();
                let provider = format!("{} ({})", self.llm.name(), self.llm.id());
                crate::ext::boot_with_tools_shared(
                    &config_dir,
                    &cwd,
                    &self.config,
                    true,
                    crate::ext::BootOptions {
                        headless: true,
                        ..Default::default()
                    },
                    &model,
                    &provider,
                    self.config.runtime_settings_handle(),
                )
            }
        };
        if !booted.manager.is_available() || !booted.source_errors.is_empty() {
            self.hub.publish(RuntimeEvent::Status {
                message: "Lua extension reload failed; previous extensions remain active".into(),
            });
            return false;
        }

        booted.manager.use_submit_inbox(self.submit_inbox.clone());
        self.extensions = booted.manager;
        if catalog_authority {
            self.config
                .replace_extension_catalog(self.extensions.extension_catalog());
        }
        {
            let mut session = self.session.lock().unwrap();
            // Definitions come from the fresh boot. Everything below is owned
            // by this conversation and must survive replacement.
            let mut tools = booted.tools;
            tools.adopt_session_state_from(&session.tools);
            session.tools = tools;
        }
        let count = self.session.lock().unwrap().tools.definitions().len();
        self.hub.publish(RuntimeEvent::Status {
            message: format!("Tools and Lua extensions reloaded. {count} tools enabled."),
        });
        self.hub.publish(frontend_state(
            &self.extensions,
            &self.session.lock().unwrap().tools,
        ));
        self.hub.publish(view_snapshot(&self.extensions));
        self.publish_snapshot();
        if reason == ReloadReason::Manual {
            self.record_sources_loaded_if_unchanged(expected_manual_source);
        }
        true
    }

    fn apply_extension_reload(&mut self, reload: ExtensionReloadRequest) {
        if self.actor_id != reload.skip_conversation_id {
            let authority = self.actor_id == Some(reload.authority_conversation_id);
            self.reload_extensions(
                authority,
                if authority {
                    ReloadReason::Manual
                } else {
                    ReloadReason::Peer
                },
            );
        }
    }

    async fn handle_host_request(
        &mut self,
        request_id: u64,
        request: bone_protocol::HostRequest,
    ) -> Flow {
        let host = self.host.clone();
        let mut response = match tokio::task::spawn_blocking(move || host.execute(request)).await {
            Ok(response) => response,
            Err(error) => bone_protocol::HostResponse::Error {
                code: bone_protocol::HostErrorCode::Internal,
                message: format!("host request failed: {error}"),
            },
        };

        let (reload_needed, setup_applied) = match &response {
            bone_protocol::HostResponse::CatalogApplied(result) => (result.changed, false),
            bone_protocol::HostResponse::SetupApplied(_) => (true, true),
            _ => (false, false),
        };
        let busy = self.hub.busy.load(std::sync::atomic::Ordering::SeqCst);
        let reloaded = reload_needed && !busy && self.reload_extensions(true, ReloadReason::Manual);
        if reload_needed && (reloaded || busy) {
            if let Some(group) = &self.hub.group
                && let Some(actor_id) = self.actor_id
            {
                group.request_extension_reload(actor_id, reloaded.then_some(actor_id));
            } else if busy {
                self.pending_commands
                    .push_back(RuntimeCommand::ReloadExtensions);
            }
        }
        if reloaded {
            let result = match &mut response {
                bone_protocol::HostResponse::CatalogApplied(result) => result,
                bone_protocol::HostResponse::SetupApplied(result) => &mut result.catalog,
                _ => unreachable!("only successful applies request reloads"),
            };
            result.extensions_reloaded = true;
        }

        self.hub.publish(RuntimeEvent::HostResponse {
            request_id,
            response,
        });
        if setup_applied {
            self.publish_config();
        }
        Flow::Continue
    }

    /// Handle one command received while the runtime is idle. Returns [`Flow`]:
    /// `Continue` once the command is fully serviced, or `StartTurn` when the
    /// command should run a model turn (`SubmitPrompt`, a submitting
    /// `RunCommand`, or a daemon background inject). `commands` is borrowed so
    /// an interactive `RunCommand` can pump replies while its handler runs.
    async fn handle_idle_command(
        &mut self,
        cmd: RuntimeCommand,
        commands: &mut mpsc::UnboundedReceiver<RuntimeCommand>,
    ) -> Flow {
        if checks_extension_sources(&cmd) {
            self.maybe_reload_changed_extensions();
        }
        match cmd {
            RuntimeCommand::SubmitPrompt {
                request_id,
                text,
                images,
            } => {
                // Finished background processes stay visible for the turn in
                // which they finished; the next user turn clears this
                // conversation's finished entries.
                let scope = crate::processes::conversation_scope(Some(
                    self.session.lock().unwrap().background_scope(),
                ));
                let _ = crate::processes::registry().clear_completed_scoped(&scope);
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
                // The Driver dispatches `message` after recognizing this
                // already-inserted prompt. Keeping lifecycle dispatch there gives
                // daemon, headless, and delegated turns one ordered path.
                Flow::StartTurn {
                    request_id,
                    text,
                    display: None,
                }
            }
            // ── Lifecycle commands (idle only) ──────────────────────────
            RuntimeCommand::NewConversation => {
                // Resetting the conversation also ends its background work —
                // it belongs to the conversation being left.
                self.cancel_background_work();
                {
                    let mut s = self.session.lock().unwrap();
                    // Already on an empty conversation? Reuse it instead of
                    // stacking another empty row (and publish a fresh snapshot
                    // below so the client still resets its view).
                    let already_empty = s.transcript.is_empty() && s.session_seq == 0;
                    if !already_empty
                        && !s.incognito
                        && let Some(db) = s.session_db.as_ref()
                    {
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
            RuntimeCommand::SetIncognito { enabled } => {
                self.set_incognito(enabled);
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
                self.cancel_background_work();
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
                let settings = self.config.runtime_settings_snapshot();
                let system_prompt =
                    crate::llm::prompts::system_prompt(settings.resolved().general.system_prompt());
                {
                    let mut s = self.session.lock().unwrap();
                    s.transcript = messages;
                    if let (Some(db), Some(conv_id)) = (s.session_db.as_ref(), s.conversation_id) {
                        let _ = db.save_context_checkpoint(conv_id, s.session_seq, &s.transcript);
                    }
                    let history = crate::chat::build_chat_history(&s.transcript, &system_prompt);
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
                let providers_config = self.config.providers_config();
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
            RuntimeCommand::GetConfig => {
                self.publish_config();
                Flow::Continue
            }
            RuntimeCommand::SetConfigValue {
                path,
                value,
                expected_revision,
                request_id,
            } => {
                let approval = (path == "general.approval")
                    .then(|| value.as_str().map(str::to_owned))
                    .flatten();
                let result = self.config.set_value(&path, value, expected_revision);
                if result.is_ok()
                    && let Some(approval) = approval
                {
                    self.set_mode(&approval);
                }
                self.finish_config_mutation(vec![path], result, false, request_id);
                Flow::Continue
            }
            RuntimeCommand::ResetConfigValue {
                path,
                expected_revision,
                request_id,
            } => {
                let result = self.config.reset_value(&path, expected_revision);
                if result.is_ok()
                    && path == "general.approval"
                    && let Some(approval) = self
                        .config
                        .snapshot()
                        .values
                        .pointer("/general/approval")
                        .and_then(serde_json::Value::as_str)
                {
                    self.set_mode(approval);
                }
                self.finish_config_mutation(vec![path], result, false, request_id);
                Flow::Continue
            }
            RuntimeCommand::UpsertProvider {
                provider,
                expected_revision,
                request_id,
            } => {
                let id = provider.id.clone();
                let candidate = self
                    .config
                    .provider_candidate_config(&provider, expected_revision)
                    .and_then(|providers| {
                        crate::llm::providers::create_provider_with_config(&id, &providers)
                            .map_err(|error| (self.config.snapshot().revision, error.to_string()))
                    });
                let candidate = match candidate {
                    Ok(candidate) => match candidate.validate().await {
                        Ok(()) => Ok(candidate),
                        Err(error) => Err((self.config.snapshot().revision, error.to_string())),
                    },
                    Err(error) => Err(error),
                };
                let (result, candidate) = match candidate {
                    Ok(candidate) => (
                        self.config.upsert_provider(provider, expected_revision),
                        Some(candidate),
                    ),
                    Err(error) => (Err(error), None),
                };
                if result.is_ok()
                    && self.llm.id() == id
                    && let Some(candidate) = candidate
                {
                    self.llm = Arc::from(candidate);
                }
                self.finish_config_mutation(
                    vec![format!("providers.{id}")],
                    result,
                    false,
                    request_id,
                );
                self.publish_snapshot();
                Flow::Continue
            }
            RuntimeCommand::DeleteProvider {
                id,
                expected_revision,
                request_id,
            } => {
                let result = self.config.delete_provider(&id, expected_revision);
                self.finish_config_mutation(
                    vec![format!("providers.{id}")],
                    result,
                    true,
                    request_id,
                );
                Flow::Continue
            }
            RuntimeCommand::SetActiveProvider {
                id,
                expected_revision,
                request_id,
            } => {
                let candidate = self
                    .config
                    .check_revision(expected_revision)
                    .and_then(|()| {
                        crate::llm::providers::create_provider_with_config(
                            &id,
                            &self.config.providers_config(),
                        )
                        .map_err(|error| (self.config.snapshot().revision, error.to_string()))
                    });
                let candidate = match candidate {
                    Ok(candidate) => match candidate.validate().await {
                        Ok(()) => Ok(candidate),
                        Err(error) => Err((self.config.snapshot().revision, error.to_string())),
                    },
                    Err(error) => Err(error),
                };
                let result = match candidate {
                    Ok(candidate) => {
                        let result = self.config.set_active_provider(&id, expected_revision);
                        if result.is_ok() {
                            self.llm = Arc::from(candidate);
                        }
                        result
                    }
                    Err(error) => Err(error),
                };
                self.finish_config_mutation(
                    vec!["providers.active".into()],
                    result,
                    false,
                    request_id,
                );
                self.publish_snapshot();
                Flow::Continue
            }
            RuntimeCommand::SetToolEnabled {
                name,
                enabled,
                expected_revision,
                request_id,
            } => {
                let result = self
                    .config
                    .set_enabled("tools", &name, enabled, expected_revision);
                if result.is_ok() {
                    self.session
                        .lock()
                        .unwrap()
                        .tools
                        .set_enabled(&name, enabled);
                }
                self.finish_config_mutation(
                    vec![format!("tools.{name}")],
                    result,
                    false,
                    request_id,
                );
                Flow::Continue
            }
            RuntimeCommand::SetCommandEnabled {
                name,
                enabled,
                expected_revision,
                request_id,
            } => {
                let result = self
                    .config
                    .set_enabled("commands", &name, enabled, expected_revision);
                self.finish_config_mutation(
                    vec![format!("commands.{name}")],
                    result,
                    false,
                    request_id,
                );
                Flow::Continue
            }
            RuntimeCommand::ReloadSettings => {
                self.reload_settings();
                Flow::Continue
            }
            RuntimeCommand::UpsertSubagent {
                agent,
                expected_revision,
                request_id,
            } => {
                let name = agent.name.clone();
                let result = self.config.upsert_subagent(agent, expected_revision);
                self.finish_subagent_change(format!("subagents.{name}"), result, request_id);
                Flow::Continue
            }
            RuntimeCommand::DeleteSubagent {
                name,
                expected_revision,
                request_id,
            } => {
                let result = self.config.delete_subagent(&name, expected_revision);
                self.finish_subagent_change(format!("subagents.{name}"), result, request_id);
                Flow::Continue
            }
            RuntimeCommand::SetSubagentEnabled {
                name,
                enabled,
                expected_revision,
                request_id,
            } => {
                let lua_agent = self
                    .extensions
                    .subagents()
                    .into_iter()
                    .find(|agent| agent.name == name && agent.source == "lua");
                let result = if let Some(mut agent) = lua_agent {
                    agent.enabled = enabled;
                    agent.source = "config".into();
                    self.config.upsert_subagent(agent, expected_revision)
                } else {
                    self.config
                        .set_subagent_enabled(&name, enabled, expected_revision)
                };
                self.finish_subagent_change(
                    format!("subagents.{name}.enabled"),
                    result,
                    request_id,
                );
                Flow::Continue
            }
            RuntimeCommand::ReloadExtensions => {
                self.reload_extensions(true, ReloadReason::Manual);
                Flow::Continue
            }
            RuntimeCommand::HostRequest {
                request_id,
                request,
            } => self.handle_host_request(request_id, request).await,
            RuntimeCommand::RunCommand {
                request_id,
                name,
                input,
            } => {
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
                            request_id,
                            output: String::new(),
                            submit: false,
                            display_role: None,
                            action: None,
                        });
                        return Flow::Continue;
                    }
                    Some(result) => result,
                };
                if name == "config" {
                    self.publish_config();
                }
                let Some(ret) = ret else {
                    self.hub.publish(RuntimeEvent::CommandComplete {
                        request_id,
                        output: String::new(),
                        submit: false,
                        display_role: None,
                        action: None,
                    });
                    let has_operations = !operations.is_empty();
                    self.apply_conversation_operations(operations);
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
                    request_id,
                    output,
                    submit,
                    display_role: ret.display_role.clone(),
                    action,
                });
                if !operations.is_empty() {
                    self.apply_conversation_operations(operations);
                    return Flow::Continue;
                }
                if submit && !ret.output.is_empty() {
                    // Submit through the normal turn path. The Driver owns the
                    // lifecycle `message` hook for command-generated prompts too.
                    {
                        let mut s = self.session.lock().unwrap();
                        s.transcript
                            .push(ChatMessage::new(crate::llm::ChatRole::User, &ret.output));
                        s.append_user_to_db(&ret.output, None);
                    }
                    // Same finished-process cleanup as a typed prompt: the
                    // command-submitted prompt is a full user turn too.
                    let scope = crate::processes::conversation_scope(Some(
                        self.session.lock().unwrap().background_scope(),
                    ));
                    let _ = crate::processes::registry().clear_completed_scoped(&scope);
                    Flow::StartTurn {
                        request_id,
                        text: ret.output,
                        display: None,
                    }
                } else {
                    self.publish_snapshot();
                    Flow::Continue
                }
            }
            RuntimeCommand::KeymapDispatch { request_id, action } => {
                let kind = self.extensions.dispatch_keymap(&action);
                self.hub
                    .publish(RuntimeEvent::KeymapDispatched { request_id, kind });
                Flow::Continue
            }
            // Lua hook on the daemon's VM; snapshot acknowledges completion.
            RuntimeCommand::DispatchHook { name, payload } => {
                let result = self
                    .run_managed_hook(commands, name.clone(), payload, false)
                    .await;
                self.apply_conversation_operations(result.operations);
                if name == "session_end" {
                    self.finalize_session_end();
                }
                self.publish_snapshot();
                Flow::Continue
            }
            // Sync terminal width from the frontend so Lua panes wrap correctly.
            RuntimeCommand::SetTerminalWidth { width } => {
                self.set_width(width);
                Flow::Continue
            }
            RuntimeCommand::CancelJob { id } => {
                self.cancel_job(&id);
                Flow::Continue
            }
            RuntimeCommand::GetProcesses => {
                self.publish_processes(true);
                Flow::Continue
            }
            RuntimeCommand::GetJobs => {
                self.publish_jobs(true);
                Flow::Continue
            }
            RuntimeCommand::Synchronize {
                request_id,
                include_messages,
            } => {
                self.publish_synchronized_state(request_id, include_messages, false);
                Flow::Continue
            }
            RuntimeCommand::CancelProcess { id } => {
                self.cancel_process(&id);
                Flow::Continue
            }
            // A cancel while idle has no turn to stop, but background work may
            // still be running — terminate it.
            RuntimeCommand::Cancel => {
                self.cancel_background_work();
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
    async fn run_turn(
        &mut self,
        request_id: Option<u64>,
        text: String,
        mut display: Option<String>,
        commands: &mut mpsc::UnboundedReceiver<RuntimeCommand>,
    ) {
        use crate::runtime::{ChannelApprovalGate, LocalConn, RuntimeConn};
        use std::sync::atomic::AtomicBool;

        let (rt_tx, rt_rx) = mpsc::unbounded_channel::<RuntimeEvent>();
        // The daemon's persistent stream carries every turn event (driver, gate,
        // Lua `ctx.ui`), so events from off-VM work that outlives the turn — an
        // idle `ctx.time.after` timer, for example — still reach attached
        // clients. The per-turn channel remains for `LocalConn`'s steer
        // acknowledgement and turn-end drain.
        let bg_tx = self.background_events_tx.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let work_timer = crate::runtime::timer::WorkTimer::start();
        self.key_registry.set_timer(Some(work_timer.clone()));
        let working_dir = self.session.lock().unwrap().tools.working_dir.clone();
        let gate = Arc::new(ChannelApprovalGate::new(
            bg_tx.clone(),
            self.approval_registry.clone(),
            Some(work_timer.clone()),
            working_dir,
        ));
        let driver = {
            let s = self.session.lock().unwrap();
            let session_sink: Arc<dyn crate::session_sink::SessionSink> = match s.conversation_id {
                Some(conversation_id) => Arc::new(
                    crate::session_sink::ToolCheckpointSessionSink::open_for(conversation_id),
                ),
                None => Arc::new(crate::session_sink::NullSessionSink),
            };
            s.build_driver(
                self.llm.clone(),
                self.extensions.clone(),
                self.config.clone(),
                self.mode.clone(),
                gate,
                bg_tx,
                self.key_registry.clone(),
                cancel.clone(),
                session_sink,
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
            request_id: None,
            text,
            images: vec![],
        });

        // Pump the turn: publish its events, and concurrently route interactive
        // replies (and cancel) from any client back into the running turn. When
        // forwarding is on, a timer drains the VM's `UiState` and forwards pane
        // diffs as events (the in-process TUI drains the shared handle itself).
        //
        // Turn events arrive on the daemon's persistent stream (see `bg_tx`);
        // this pump drains it while the turn runs, and the daemon loop's select
        // drains it afterwards — which is how a recap notice fired by an idle
        // timer reaches clients that are still attached.
        let mut diff_timer = tokio::time::interval(std::time::Duration::from_millis(50));
        loop {
            tokio::select! {
                biased;
                background_ev = self.background_events_rx.recv() => {
                    if let Some(mut ev) = background_ev {
                        if let RuntimeEvent::Started {
                            request_id: event_request_id,
                            display: event_display,
                            ..
                        } = &mut ev
                        {
                            *event_request_id = request_id;
                            *event_display = display.take();
                        }
                        self.publish_runtime_event(ev);
                    }
                }
                ev = conn.next_event() => match ev {
                    // Steer acknowledgements and the turn-end drain of the
                    // per-turn channel.
                    Some(ev) => self.publish_runtime_event(ev),
                    None => break, // turn drained
                },
                _ = diff_timer.tick() => {
                    self.publish_processes(false);
                    self.publish_jobs(false);
                    if self.forward_view_diffs {
                        self.drain_diffs();
                    }
                },
                cmd = commands.recv() => match cmd {
                    // A turn cancel also terminates the session's background
                    // sub-agents and managed shell processes: they were spawned
                    // by this conversation, so Ctrl+C should stop them too rather
                    // than leave them running after the user abandoned the turn.
                    Some(cmd @ RuntimeCommand::Cancel) => {
                        self.cancel_background_work();
                        self.pending_interactions.clear();
                        conn.send(cmd);
                    }
                    Some(cmd @ RuntimeCommand::ApprovalReply { id, .. }) => {
                        self.pending_interactions
                            .remove(InteractionId::Approval(id));
                        conn.send(cmd);
                    }
                    Some(cmd @ RuntimeCommand::KeyReply { id, .. }) => {
                        self.pending_interactions.remove(InteractionId::Key(id));
                        conn.send(cmd);
                    }
                    Some(RuntimeCommand::CancelJob { id }) => self.cancel_job(&id),
                    Some(RuntimeCommand::GetProcesses) => self.publish_processes(true),
                    Some(RuntimeCommand::GetJobs) => self.publish_jobs(true),
                    Some(RuntimeCommand::Synchronize {
                        request_id,
                        include_messages,
                    }) => self.publish_synchronized_state(request_id, include_messages, true),
                    Some(RuntimeCommand::CancelProcess { id }) => self.cancel_process(&id),
                    // Mid-turn Safe/Danger toggle: applies to the rest of the turn
                    // (the gate reads the shared atomic per call).
                    Some(RuntimeCommand::SetApprovalMode { mode: mode_str }) => self.persist_mode(&mode_str),
                    Some(RuntimeCommand::ReloadSettings) => self.reload_settings(),
                    // Preserve prompts received while a turn is active. They are
                    // handled through the normal idle path after this turn, so
                    // transcript insertion, persistence, hooks, and turn ordering
                    // remain identical to an idle submission.
                    Some(cmd @ RuntimeCommand::SubmitPrompt { .. }) => {
                        self.pending_commands.push_back(cmd)
                    }
                    // Width updates are safe mid-turn. Hooks need the idle
                    // daemon/session owner so their full context and mutations are
                    // applied in lifecycle order. Ending the conversation remains
                    // rejected while the active Driver can still persist.
                    Some(RuntimeCommand::SetTerminalWidth { width }) => self.set_width(width),
                    Some(RuntimeCommand::DispatchHook { name, .. }) if name == "session_end" => {
                        self.hub.publish(RuntimeEvent::Status {
                            message: "busy: cannot end the conversation during a turn".into(),
                        });
                    }
                    Some(command @ RuntimeCommand::DispatchHook { .. }) => {
                        self.pending_commands.push_back(command)
                    }
                    Some(cmd @ RuntimeCommand::Steer { .. }) => conn.send(cmd),
                    Some(cmd) if is_config_command(&cmd) => {
                        let _ = self.handle_idle_command(cmd, commands).await;
                    }
                    // Requests that require the idle Lua/session owner (slash
                    // commands, keymaps, lifecycle changes, etc.) must not
                    // disappear merely because another client has a live turn.
                    Some(command) => self.pending_commands.push_back(command),
                    None => break,
                },
            }
        }
        self.pending_interactions.clear();
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
            let (_, persistence_error) = self.session.lock().unwrap().apply_outcome(outcome);
            if let Some(err) = persistence_error {
                self.hub.publish(RuntimeEvent::Status {
                    message: format!("failed to persist turn: {err}"),
                });
            }
        }
        // Publish the post-turn state so clients can sync their view-model.
        self.key_registry.set_timer(None);
        self.publish_snapshot();
        self.hub.publish(RuntimeEvent::WorkElapsed {
            elapsed_ms: work_timer.elapsed_ms(),
        });
        match request_id {
            Some(request_id) => self.hub.publish(RuntimeEvent::TurnCompleted { request_id }),
            None => self.hub.publish(RuntimeEvent::TurnComplete),
        }
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
    commands: mpsc::UnboundedReceiver<RuntimeCommand>,
    llm: Arc<dyn crate::llm::provider::LlmProvider>,
    extensions: crate::ext::ExtensionManager,
    config: crate::config::store::ConfigStore,
    session: Arc<Mutex<crate::runtime::RuntimeSession>>,
    approval_mode: crate::tools::ApprovalMode,
    // Optional single-boot handoff from an in-process frontend.
    reload_inbox: Option<Arc<Mutex<Option<crate::ext::BootedTools>>>>,
    // Whether the daemon forwards Lua view diffs to clients.
    forward_view_diffs: bool,
) {
    run_daemon_inner(
        hub.into(),
        commands,
        llm,
        extensions,
        config,
        session,
        approval_mode,
        reload_inbox,
        forward_view_diffs,
        None,
    )
    .await;
}

/// Run a daemon actor whose live provider/extension projection is also used by
/// a [`ManagedRuntime`] attachment replay.
#[allow(clippy::too_many_arguments)]
pub async fn run_daemon_with_projection(
    hub: impl Into<HubPublisher>,
    commands: mpsc::UnboundedReceiver<RuntimeCommand>,
    llm: Arc<dyn crate::llm::provider::LlmProvider>,
    extensions: crate::ext::ExtensionManager,
    config: crate::config::store::ConfigStore,
    session: Arc<Mutex<crate::runtime::RuntimeSession>>,
    approval_mode: crate::tools::ApprovalMode,
    reload_inbox: Option<Arc<Mutex<Option<crate::ext::BootedTools>>>>,
    forward_view_diffs: bool,
    projection: RuntimeProjection,
) {
    run_daemon_inner(
        hub.into(),
        commands,
        llm,
        extensions,
        config,
        session,
        approval_mode,
        reload_inbox,
        forward_view_diffs,
        Some(projection),
    )
    .await;
}

async fn next_extension_reload(
    receiver: &mut Option<tokio::sync::watch::Receiver<ExtensionReloadRequest>>,
) -> Option<ExtensionReloadRequest> {
    let receiver = receiver.as_mut()?;
    receiver.changed().await.ok()?;
    Some(*receiver.borrow_and_update())
}

fn take_pending_extension_reload(
    receiver: &mut Option<tokio::sync::watch::Receiver<ExtensionReloadRequest>>,
) -> Option<ExtensionReloadRequest> {
    let receiver = receiver.as_mut()?;
    receiver
        .has_changed()
        .ok()
        .filter(|changed| *changed)
        .map(|_| *receiver.borrow_and_update())
}

#[allow(clippy::too_many_arguments)]
async fn run_daemon_inner(
    hub: HubPublisher,
    mut commands: mpsc::UnboundedReceiver<RuntimeCommand>,
    llm: Arc<dyn crate::llm::provider::LlmProvider>,
    extensions: crate::ext::ExtensionManager,
    config: crate::config::store::ConfigStore,
    session: Arc<Mutex<crate::runtime::RuntimeSession>>,
    approval_mode: crate::tools::ApprovalMode,
    reload_inbox: Option<Arc<Mutex<Option<crate::ext::BootedTools>>>>,
    forward_view_diffs: bool,
    projection: Option<RuntimeProjection>,
) {
    let mut extension_reloads = hub
        .group
        .as_ref()
        .map(HubGroup::subscribe_extension_reloads);
    let host = hub
        .group
        .as_ref()
        .map(|group| group.host_service(config.clone()))
        .unwrap_or_else(|| crate::host::HostService::new(config.clone()));
    let submit_inbox = extensions.submit_inbox();
    let actor_id = session
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .conversation_id;
    let extension_sources = hub
        .group
        .as_ref()
        .map(|group| group.0.extension_sources.clone())
        .unwrap_or_else(|| Arc::new(Mutex::new(ExtensionSourceState::default())));
    let background_events = mpsc::unbounded_channel::<RuntimeEvent>();
    let mut ctx = DaemonCtx {
        hub,
        llm,
        extensions,
        submit_inbox,
        session,
        actor_id,
        mode: crate::tools::SharedApprovalMode::new(approval_mode),
        approval_registry: crate::runtime::ApprovalReplyRegistry::new(),
        key_registry: crate::runtime::KeyReplyRegistry::new(),
        pending_interactions: PendingInteractions::default(),
        pending_commands: std::collections::VecDeque::new(),
        reload_inbox,
        extension_sources,
        forward_view_diffs,
        config,
        host,
        processes_seen: None,
        jobs_seen: None,
        projection,
        background_events_tx: background_events.0,
        background_events_rx: background_events.1,
    };
    ctx.initialize_current_sources();
    ctx.refresh_projection();
    ctx.publish_config();

    // Each command is serviced by `handle_idle_command`; commands that start a
    // model turn return `StartTurn`, which `run_turn` builds and pumps to
    // completion. An idle poll always drains background sub-agent results and
    // Lua-submitted prompts (`bone.submit`) so injection is daemon-owned for
    // both the in-process TUI and remote clients.
    let mut inject_timer = tokio::time::interval(std::time::Duration::from_millis(200));
    loop {
        let mut turn_guard = None;
        let flow = if let Some(reload) = take_pending_extension_reload(&mut extension_reloads) {
            ctx.apply_extension_reload(reload);
            Flow::Continue
        } else if let Some(cmd) = ctx.pending_commands.pop_front() {
            turn_guard = starts_turn(&cmd).then(|| ctx.hub.begin_turn());
            ctx.handle_idle_command(cmd, &mut commands).await
        } else {
            tokio::select! {
                biased;
                reload = next_extension_reload(&mut extension_reloads), if extension_reloads.is_some() => {
                    match reload {
                        Some(reload) => {
                            ctx.apply_extension_reload(reload);
                            Flow::Continue
                        }
                        None => {
                            extension_reloads = None;
                            Flow::Continue
                        }
                    }
                },
                cmd = commands.recv() => match cmd {
                    Some(cmd) => {
                        turn_guard = starts_turn(&cmd).then(|| ctx.hub.begin_turn());
                        ctx.handle_idle_command(cmd, &mut commands).await
                    }
                    None => break,
                },
                // Drain events that landed after the previous turn ended (e.g. an
                // idle `ctx.time.after` recap notice). The daemon holds a keep-alive
                // sender, so this arm never resolves with `None`.
                background_ev = ctx.background_events_rx.recv() => {
                    if let Some(ev) = background_ev {
                        ctx.publish_runtime_event(ev);
                    }
                    Flow::Continue
                },
                _ = inject_timer.tick() => {
                    ctx.publish_processes(false);
                    ctx.publish_jobs(false);
                    match ctx.next_background_prompt() {
                        // Route through the same `SubmitPrompt` handling as a typed
                        // prompt (transcript push, DB persist, `message` hook), then
                        // attach the short display label for job-result injects.
                        Some((text, display)) => {
                            turn_guard = Some(ctx.hub.begin_turn());
                            match ctx
                                .handle_idle_command(
                                    RuntimeCommand::SubmitPrompt {
                                        request_id: None,
                                        text,
                                        images: vec![],
                                    },
                                    &mut commands,
                                )
                                .await
                            {
                                Flow::StartTurn {
                                    request_id, text, ..
                                } => Flow::StartTurn {
                                    request_id,
                                    text,
                                    display,
                                },
                                other => other,
                            }
                        }
                        None => Flow::Continue,
                    }
                }
            }
        };
        if let Flow::StartTurn {
            request_id,
            text,
            display,
        } = flow
        {
            ctx.run_turn(request_id, text, display, &mut commands).await;
        }
        drop(turn_guard);
    }
}

#[cfg(test)]
#[path = "rpc_tests.rs"]
mod rpc_tests;

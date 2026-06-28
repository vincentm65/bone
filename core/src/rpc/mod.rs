//! RPC transport for the runtime protocol.
//!
//! Carries [`RuntimeEvent`] (core → frontend) and [`RuntimeCommand`]
//! (frontend → core) over a byte stream as newline-delimited JSON. The same
//! `serde` types flow over an in-process channel (Phase 3) and here over a
//! socket — only the framing differs. (msgpack via `rmpv` could replace the
//! JSONL codec later without touching the protocol types.)
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

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{broadcast, mpsc};

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

/// Build the [`RuntimeEvent::FrontendState`] carrying the daemon VM's boot-time
/// display state (theme/keymap/banner/commands/config) for a VM-less frontend.
/// Snapshots are serialized to JSON so the protocol crate stays free of the
/// core's Lua snapshot types; the client deserializes them back.
pub fn frontend_state(
    extensions: &crate::ext::ExtensionManager,
    tools: &crate::tools::registry::ToolHandler,
) -> RuntimeEvent {
    RuntimeEvent::FrontendState {
        banner: extensions.frontend_banner(),
        theme: serde_json::to_value(extensions.theme_snapshot()).unwrap_or_default(),
        keymap: serde_json::to_value(extensions.keymap_snapshot()).unwrap_or_default(),
        config: serde_json::to_value(extensions.config_snapshot()).unwrap_or_default(),
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
    fn publish_snapshot(&self) {
        self.hub.publish(RuntimeEvent::StateSnapshot {
            snapshot: {
                let s = self.session.lock().unwrap();
                let mut snap = s.snapshot(self.llm.id(), self.llm.model());
                snap.usage_by_provider = crate::ext::ctx::usage_by_provider_context(
                    s.session_db.as_ref(),
                    s.conversation_id,
                );
                snap
            },
        });
    }

    /// Forward any pane/UI diffs the Lua VM has queued to remote frontends.
    fn drain_diffs(&self) {
        for diff in self.extensions.drain_view_diffs() {
            self.hub.publish(RuntimeEvent::ViewDiff { diff });
        }
    }

    /// Apply a Safe/Danger toggle. The gate reads the shared atomic per call, so
    /// this takes effect immediately — even mid-turn.
    fn set_mode(&self, mode_str: &str) {
        self.mode.set(match mode_str {
            "danger" => crate::tools::ApprovalMode::Danger,
            _ => crate::tools::ApprovalMode::Safe,
        });
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
    /// *remote* frontend run interactive commands against the daemon's Lua VM
    /// instead of a local one (Phase 3-pure). The in-process TUI keeps running
    /// commands locally against the shared VM.
    async fn run_interactive_command(
        &self,
        commands: &mut mpsc::UnboundedReceiver<RuntimeCommand>,
        name: String,
        input: String,
    ) -> Option<Option<crate::ext::types::LuaCommandReturn>> {
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
                by_provider,
                s.transcript.clone(),
            )
        };

        let lua = self.extensions.lua_handle();
        let shared_ui = self.extensions.ui_handle();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_for_ctx = cancel.clone();
        let (live_tx, mut live_rx) =
            mpsc::unbounded_channel::<crate::tools::types::ToolLiveEvent>();

        // The handler call blocks (Lua + nested tool calls), so run it off the
        // async runtime. Mirrors the TUI's spawn_blocking in `run_lua_command`.
        // Outer `Option` = "was the command found?"; inner `Option` = the parsed
        // result (a found handler may legitimately return a no-op `None`).
        let mut handle = tokio::task::spawn_blocking(move || {
            let lua_guard = lua.lock().unwrap_or_else(|e| e.into_inner());
            // Not found: the only case that should surface as "unknown command".
            let handler = crate::ext::ops_commands::find_handler(&lua_guard, &name)?;
            let config_dir = crate::config::bone_dir().to_string_lossy().to_string();
            let shared_state = crate::ext::ctx::process_shared_state();
            let mut ctx_cfg = crate::ext::ctx::CtxConfig::new(config_dir, shared_state);
            app_state.apply_to(&mut ctx_cfg);
            ctx_cfg.pane_sender = Some(live_tx);
            ctx_cfg.ui = Some(shared_ui);
            ctx_cfg.cancelled = Some(cancel_for_ctx);
            // The handler exists; from here every outcome is `Some(_)` so the daemon
            // never mistakes a ran command for an unknown one.
            let ctx_table = match crate::ext::ctx::create_ctx_table(&lua_guard, &ctx_cfg) {
                Ok(t) => t,
                Err(_) => return Some(None),
            };
            // Release the VM lock before calling in: a nested `ctx.tools.call` runs
            // inline on this thread and must re-acquire the (non-reentrant) mutex.
            drop(lua_guard);
            Some(match handler.call::<mlua::Value>((input, ctx_table)) {
                Ok(value) => crate::ext::types::parse_lua_command_return(value),
                Err(e) => Some(crate::ext::types::LuaCommandReturn {
                    output: format!("Lua command error: {e}"),
                    submit: false,
                    action: None,
                    display_role: None,
                }),
            })
        });

        let mut diff_timer = tokio::time::interval(std::time::Duration::from_millis(50));
        loop {
            tokio::select! {
                res = &mut handle => {
                    // Flush any trailing pane diffs the handler emitted.
                    self.drain_diffs();
                    return res.ok().flatten();
                }
                Some(crate::tools::types::ToolLiveEvent::Key(req)) = live_rx.recv() => {
                    let id = self.key_registry.register(req);
                    self.hub.publish(RuntimeEvent::KeyRequest { id });
                }
                _ = diff_timer.tick() => self.drain_diffs(),
                cmd = commands.recv() => match cmd {
                    Some(RuntimeCommand::KeyReply { id, key }) => { self.key_registry.resolve(id, key); }
                    Some(RuntimeCommand::Cancel) => cancel.store(true, Ordering::Relaxed),
                    Some(_) => {} // other commands are ignored while a command runs
                    None => cancel.store(true, Ordering::Relaxed),
                },
            }
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
                {
                    let mut s = self.session.lock().unwrap();
                    if let Some(db) = s.session_db.as_ref() {
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
                self.publish_snapshot();
                Flow::Continue
            }
            RuntimeCommand::LoadConversation { id } => {
                let messages = {
                    let s = self.session.lock().unwrap();
                    s.session_db.as_ref().and_then(|db| {
                        db.list_messages(id, 1000).ok().map(|rows| {
                            rows.into_iter()
                                .map(crate::session_db::stored_to_chat_message)
                                .collect::<Vec<_>>()
                        })
                    })
                };
                if let Some(messages) = messages {
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
                        s.transcript = messages.clone();
                        s.token_stats.reset();
                        let mut snap = s.snapshot(self.llm.id(), self.llm.model());
                        // `snapshot` leaves usage_by_provider empty; fill it from
                        // the DB so the client's /usage table isn't blanked on
                        // load (mirrors `publish_snapshot`).
                        snap.usage_by_provider = crate::ext::ctx::usage_by_provider_context(
                            s.session_db.as_ref(),
                            s.conversation_id,
                        );
                        snap
                    };
                    self.hub
                        .publish(RuntimeEvent::ConversationLoaded { messages, snapshot });
                } else {
                    self.hub.publish(RuntimeEvent::Status {
                        message: format!("failed to load conversation {id}"),
                    });
                }
                Flow::Continue
            }
            RuntimeCommand::SetApprovalMode { mode: mode_str } => {
                self.set_mode(&mode_str);
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
                {
                    let mut s = self.session.lock().unwrap();
                    s.transcript.clear();
                    s.token_stats.reset();
                }
                self.publish_snapshot();
                Flow::Continue
            }
            RuntimeCommand::ReplaceConversation { messages } => {
                {
                    let mut s = self.session.lock().unwrap();
                    s.transcript = messages;
                    s.token_stats.reset();
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
                    Ok(new_provider) => self.llm = Arc::from(new_provider),
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
                    s.tools = booted.tools;
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
                let ret = match result {
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
                    // Handler ran but produced no output/action (e.g. an
                    // interactive picker returning `{ submit = false }`). Complete
                    // the command cleanly — NOT an error. Without this, a no-op
                    // return was mis-reported as "unknown command: <name>" even
                    // though the picker worked. Mirrors the local path, which
                    // treats a found-but-no-op handler as "handled, just redraw".
                    Some(None) => {
                        self.hub.publish(RuntimeEvent::CommandComplete {
                            output: String::new(),
                            submit: false,
                            display_role: None,
                            action: None,
                        });
                        self.publish_snapshot();
                        return Flow::Continue;
                    }
                    Some(Some(ret)) => ret,
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
                let submit = ret.submit && !reply_bearing;
                let action = ret.action.as_ref().and_then(|a| a.to_command_action());
                self.hub.publish(RuntimeEvent::CommandComplete {
                    output: ret.output.clone(),
                    submit,
                    display_role: ret.display_role.clone(),
                    action,
                });
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
            // Acknowledge other non-turn commands so a client isn't left waiting.
            other => {
                if !matches!(other, RuntimeCommand::Cancel) {
                    self.hub.publish(RuntimeEvent::Status {
                        message: format!("ignored (idle): {other:?}"),
                    });
                }
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
        let gate = Arc::new(ChannelApprovalGate::new(
            rt_tx.clone(),
            self.approval_registry.clone(),
        ));
        let (persist_from, driver) = {
            let s = self.session.lock().unwrap();
            let pf = s.transcript.len();
            let d = s.build_driver(
                self.llm.clone(),
                self.extensions.clone(),
                self.mode.clone(),
                gate,
                rt_tx,
                self.key_registry.clone(),
                cancel.clone(),
                Arc::new(crate::session_sink::NullSessionSink),
            );
            (pf, d)
        };
        let mut conn = LocalConn::new(
            rt_rx,
            driver,
            cancel,
            self.approval_registry.clone(),
            self.key_registry.clone(),
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
                    Some(cmd @ (RuntimeCommand::ApprovalReply { .. }
                    | RuntimeCommand::KeyReply { .. }
                    | RuntimeCommand::Cancel)) => conn.send(cmd),
                    // Mid-turn Safe/Danger toggle: applies to the rest of the turn
                    // (the gate reads the shared atomic per call).
                    Some(RuntimeCommand::SetApprovalMode { mode: mode_str }) => self.set_mode(&mode_str),
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
                    Some(_) => {}
                    None => break,
                },
            }
        }
        // Flush any diffs emitted between the last tick and turn end.
        if self.forward_view_diffs {
            self.drain_diffs();
        }

        if let Some(outcome) = conn.take_outcome() {
            let _ = self
                .session
                .lock()
                .unwrap()
                .apply_outcome(outcome, persist_from);
        }
        // Publish the post-turn state so clients can sync their view-model.
        self.publish_snapshot();
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
    // which `run_turn` builds and pumps to completion.
    while let Some(cmd) = commands.recv().await {
        if let Flow::StartTurn(text) = ctx.handle_idle_command(cmd, &mut commands).await {
            ctx.run_turn(text, &mut commands).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn publisher_does_not_keep_command_channel_open() {
        let (hub, mut commands_rx) = Hub::new();
        let publisher = hub.publisher();

        drop(hub);

        let received = tokio::time::timeout(std::time::Duration::from_secs(1), commands_rx.recv())
            .await
            .expect("command receiver stayed open");
        assert!(received.is_none());

        // The runtime-facing half remains usable without retaining a command
        // sender, even when there are no event subscribers.
        publisher.publish(RuntimeEvent::Status {
            message: "no listeners".into(),
        });
    }

    #[tokio::test]
    async fn dropping_remote_client_closes_its_transport() {
        let (client_io, mut peer_io) = tokio::io::duplex(4096);
        let (read_half, write_half) = tokio::io::split(client_io);
        let client = RemoteClient::connect(read_half, write_half);

        drop(client);

        let mut byte = [0_u8; 1];
        let read = tokio::time::timeout(std::time::Duration::from_secs(1), peer_io.read(&mut byte))
            .await
            .expect("remote bridge kept the transport open")
            .unwrap();
        assert_eq!(read, 0, "peer should observe EOF after client drop");
    }

    #[tokio::test]
    async fn hub_fans_out_events_and_merges_commands() {
        let (hub, mut commands_rx) = Hub::new();

        // Two clients connected by in-memory duplex pipes.
        let (client_a, server_a) = tokio::io::duplex(4096);
        let (client_b, server_b) = tokio::io::duplex(4096);
        tokio::spawn(serve_connection(server_a, hub.clone(), vec![]));
        tokio::spawn(serve_connection(
            server_b,
            hub.clone(),
            vec![RuntimeEvent::Status {
                message: "welcome".into(),
            }],
        ));

        // Give the writer tasks a moment to subscribe before broadcasting.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(hub.client_count(), 2);

        // Broadcast an event; both clients receive it.
        hub.publish(RuntimeEvent::Finished {
            content: "done".into(),
        });

        let mut ra = codec::MessageReader::new(tokio::io::split(client_a).0);
        let ev_a: RuntimeEvent = ra.read().await.unwrap().unwrap();
        assert!(matches!(ev_a, RuntimeEvent::Finished { content } if content == "done"));

        // Client B saw its initial welcome first, then the broadcast.
        let mut rb = codec::MessageReader::new(tokio::io::split(client_b).0);
        let ev_b0: RuntimeEvent = rb.read().await.unwrap().unwrap();
        assert!(matches!(ev_b0, RuntimeEvent::Status { message } if message == "welcome"));
        let ev_b1: RuntimeEvent = rb.read().await.unwrap().unwrap();
        assert!(matches!(ev_b1, RuntimeEvent::Finished { .. }));

        // A client writes a command; the hub surfaces it on the merged stream.
        let (client_c, server_c) = tokio::io::duplex(4096);
        tokio::spawn(serve_connection(server_c, hub.clone(), vec![]));
        let mut wc = tokio::io::split(client_c).1;
        codec::write_message(
            &mut wc,
            &RuntimeCommand::SubmitPrompt {
                text: "hi".into(),
                images: vec![],
            },
        )
        .await
        .unwrap();

        let cmd = commands_rx.recv().await.unwrap();
        assert!(matches!(cmd, RuntimeCommand::SubmitPrompt { text, .. } if text == "hi"));
    }

    #[tokio::test]
    async fn malformed_frame_is_skipped_not_fatal() {
        let (hub, mut commands_rx) = Hub::new();
        let (client, server) = tokio::io::duplex(4096);
        tokio::spawn(serve_connection(server, hub.clone(), vec![]));

        let mut w = tokio::io::split(client).1;
        // Garbage line, then a valid command on the next line.
        w.write_all(b"{not valid json}\n").await.unwrap();
        codec::write_message(&mut w, &RuntimeCommand::Cancel)
            .await
            .unwrap();

        let cmd = commands_rx.recv().await.unwrap();
        assert!(matches!(cmd, RuntimeCommand::Cancel));
    }
}

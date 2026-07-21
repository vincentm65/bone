//! How the turn protocol is pumped at a transport edge.
//!
//! A [`RuntimeConn`] pushes [`RuntimeCommand`]s in and pulls [`RuntimeEvent`]s
//! back out for one agent turn. Two implementations carry the exact same
//! protocol, but they sit on *opposite* sides of it:
//!
//! - [`LocalConn`] — the runtime side. It owns the in-flight `Driver` future
//!   and polls it on the runtime's own task, so there is no spawn and the Lua
//!   VM is never touched while a tool runs. `run_daemon` uses this for every
//!   turn — including the in-process `bone`, whose daemon runs on the same
//!   `LocalSet` as the TUI.
//! - [`SocketConn`] — the remote-client side. The same events/commands cross a
//!   socket via the JSONL `rpc::codec`. Used by `rpc::RemoteClient` (the TUI's
//!   transport in `--connect` mode) and the `bone connect` reference client.
//!
//! Note this is **not** the seam the TUI talks through. A frontend pushes
//! commands and pulls events over the `rpc::Hub` (in-process) or `RemoteClient`
//! (remote) channels; `RuntimeConn` lives one layer below — between the daemon
//! and the Driver, and between a remote client and its socket. The shared
//! protocol is what makes the TUI a *client* either way (the Neovim model);
//! `RuntimeConn` is just how that protocol is moved at the transport edges.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;

use crate::rpc::codec;
use crate::runtime::driver::{Driver, DriverOutcome};
use crate::runtime::{ApprovalReplyRegistry, KeyReplyRegistry, RuntimeCommand, RuntimeEvent};

/// Pumps the turn protocol at a transport edge: push commands, pull events.
/// The same trait backs the runtime-side [`LocalConn`] (which drives the
/// `Driver`) and the client-side [`SocketConn`] (which talks to a remote
/// daemon). It is not what the TUI holds — see the module docs.
pub trait RuntimeConn {
    /// Push a command to the runtime (submit a prompt, answer an approval/key
    /// request, cancel the turn).
    fn send(&mut self, cmd: RuntimeCommand);

    /// Pull the next event. `None` means the current turn has fully drained
    /// (the runtime is idle) — the frontend loop should stop pumping events.
    ///
    /// Not `Send`: the in-process [`LocalConn`] holds the Driver future, which
    /// owns the Lua VM handle and is polled on the runtime's own task (never
    /// spawned), so it need not cross threads.
    fn next_event(&mut self) -> impl Future<Output = Option<RuntimeEvent>>;
}

/// In-process runtime connection: owns the `Driver` future and drives it on the
/// runtime's own task (the `run_daemon` loop, or the in-process `bone`'s
/// `LocalSet`).
///
/// `send(SubmitPrompt)` starts the turn; `next_event()` interleaves the Driver's
/// streamed events with the future's completion. When the future finishes its
/// [`DriverOutcome`] is stashed for the runtime to reabsorb via
/// [`take_outcome`]. Approval/key replies resolve the shared registries the
/// turn's `ChannelApprovalGate` / `ctx.ui.key` block on; `Cancel` flips the flag
/// and drains both registries so no interactive waiter can wedge the turn.
///
/// [`take_outcome`]: LocalConn::take_outcome
pub struct LocalConn {
    events_rx: mpsc::UnboundedReceiver<RuntimeEvent>,
    /// Sender for emitting status/events (e.g. steer acknowledgement).
    events_tx: mpsc::UnboundedSender<RuntimeEvent>,
    /// The Driver awaiting a `SubmitPrompt` to start; consumed into `run_fut`.
    driver: Option<Driver>,
    /// The in-flight turn future; `None` before submit and after completion.
    run_fut: Option<Pin<Box<dyn Future<Output = DriverOutcome> + Send>>>,
    /// The completed turn's reclaimable state, awaiting `take_outcome`.
    outcome: Option<DriverOutcome>,
    cancel: Arc<AtomicBool>,
    approval_registry: ApprovalReplyRegistry,
    key_registry: KeyReplyRegistry,
    turn_nudge: Arc<Mutex<Option<String>>>,
}

impl LocalConn {
    /// Build a connection around a turn's `Driver` and its event stream. The
    /// `cancel` flag and the two registries must be the same instances wired
    /// into the `Driver`/gate so `send` reaches the running turn.
    pub fn new(
        events_rx: mpsc::UnboundedReceiver<RuntimeEvent>,
        events_tx: mpsc::UnboundedSender<RuntimeEvent>,
        driver: Driver,
        cancel: Arc<AtomicBool>,
        approval_registry: ApprovalReplyRegistry,
        key_registry: KeyReplyRegistry,
        turn_nudge: Arc<Mutex<Option<String>>>,
    ) -> Self {
        Self {
            events_rx,
            events_tx,
            driver: Some(driver),
            run_fut: None,
            outcome: None,
            cancel,
            approval_registry,
            key_registry,
            turn_nudge,
        }
    }

    /// Take the completed turn's reclaimable state. `Some` only once the turn
    /// future has finished (i.e. after `next_event` has returned `None`).
    pub fn take_outcome(&mut self) -> Option<DriverOutcome> {
        self.outcome.take()
    }

    /// True once the turn future has completed (outcome captured).
    pub fn is_finished(&self) -> bool {
        self.run_fut.is_none() && self.driver.is_none()
    }

    fn start(&mut self, prompt: String) {
        if let Some(driver) = self.driver.take() {
            self.run_fut = Some(driver.into_turn_future(prompt));
        }
    }
}

impl RuntimeConn for LocalConn {
    fn send(&mut self, cmd: RuntimeCommand) {
        match cmd {
            RuntimeCommand::SubmitPrompt { text, .. } => self.start(text),
            RuntimeCommand::ApprovalReply { id, outcome } => {
                self.approval_registry.resolve(id, outcome);
            }
            RuntimeCommand::KeyReply { id, key } => {
                self.key_registry.resolve(id, key);
            }
            RuntimeCommand::Cancel => {
                self.cancel.store(true, Ordering::Relaxed);
                self.approval_registry.cancel_all();
                self.key_registry.cancel_all();
            }
            RuntimeCommand::Steer { text } => {
                // Stash the steer text in the shared `turn_nudge`; the driver
                // consumes it at the top of its next loop iteration and injects
                // it as a transient `turn_message` (never persisted to the
                // transcript). This does not mutate history — it shapes only the
                // next provider request.
                self.turn_nudge.lock().unwrap().replace(text.clone());
                let _ = self.events_tx.send(RuntimeEvent::Status {
                    message: format!("steered: {text}"),
                });
            }
            // Daemon-level commands are not part of a single in-process turn.
            RuntimeCommand::RunCommand { .. }
            | RuntimeCommand::CancelJob { .. }
            | RuntimeCommand::NewConversation
            | RuntimeCommand::LoadConversation { .. }
            | RuntimeCommand::ClearConversation
            | RuntimeCommand::SwitchProvider { .. }
            | RuntimeCommand::ReplaceConversation { .. }
            | RuntimeCommand::SetApprovalMode { .. }
            | RuntimeCommand::AppendMessage { .. }
            | RuntimeCommand::DispatchHook { .. }
            | RuntimeCommand::SetTerminalWidth { .. }
            | RuntimeCommand::KeymapDispatch { .. }
            | RuntimeCommand::ReloadSettings
            | RuntimeCommand::SetSetting { .. }
            | RuntimeCommand::UpsertSubagent { .. }
            | RuntimeCommand::DeleteSubagent { .. }
            | RuntimeCommand::SetSubagentEnabled { .. }
            | RuntimeCommand::ReloadExtensions => {}
        }
    }

    async fn next_event(&mut self) -> Option<RuntimeEvent> {
        match self.run_fut.as_mut() {
            Some(fut) => {
                tokio::select! {
                    outcome = fut => {
                        // Turn finished. Its events were all sent (synchronously,
                        // over an unbounded channel) before it returned, so drain
                        // any still buffered before signalling idle with `None`.
                        self.outcome = Some(outcome);
                        self.run_fut = None;
                        self.approval_registry.cancel_all();
                        self.key_registry.cancel_all();
                        self.events_rx.try_recv().ok()
                    }
                    Some(ev) = self.events_rx.recv() => Some(ev),
                }
            }
            // No future running: either it never started, or it completed and we
            // are draining the trailing buffered events, then idle (`None`).
            None => self.events_rx.try_recv().ok(),
        }
    }
}

/// Remote runtime connection: the same protocol over a socket to `bone serve`.
///
/// A background task owns the write half and drains `send`-queued commands, so
/// `send` stays non-blocking and sync (matching [`LocalConn`]); `next_event`
/// decodes the event stream off the read half. Unlike `LocalConn`, `None` from
/// `next_event` means the *connection closed* (not turn-idle): a remote frontend
/// detects turn end from a `Finished`/`Failed` event and keeps the connection
/// open across turns, since the runtime lives in the daemon.
pub struct SocketConn<R> {
    reader: codec::MessageReader<R>,
    cmd_tx: mpsc::UnboundedSender<RuntimeCommand>,
    writer: tokio::task::JoinHandle<()>,
}

impl<R> SocketConn<R>
where
    R: AsyncRead + Unpin,
{
    /// Build a connection from the split halves of a duplex stream. The write
    /// half is moved into a writer task; commands queued via `send` are framed
    /// and flushed to it in order.
    pub fn new<W>(read_half: R, write_half: W) -> Self
    where
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<RuntimeCommand>();
        let writer = tokio::spawn(async move {
            let mut w = write_half;
            while let Some(cmd) = cmd_rx.recv().await {
                if codec::write_message(&mut w, &cmd).await.is_err() {
                    break;
                }
            }
        });
        Self {
            reader: codec::MessageReader::new(read_half),
            cmd_tx,
            writer,
        }
    }

    /// A cloneable handle for queuing commands without borrowing the connection
    /// — lets a client push prompts/replies from a `select!` arm while another
    /// arm holds `&mut self` in [`next_event`](RuntimeConn::next_event).
    pub fn command_sender(&self) -> mpsc::UnboundedSender<RuntimeCommand> {
        self.cmd_tx.clone()
    }
}

impl<R> Drop for SocketConn<R> {
    fn drop(&mut self) {
        self.writer.abort();
    }
}

impl<R> RuntimeConn for SocketConn<R>
where
    R: AsyncRead + Unpin,
{
    fn send(&mut self, cmd: RuntimeCommand) {
        let _ = self.cmd_tx.send(cmd);
    }

    async fn next_event(&mut self) -> Option<RuntimeEvent> {
        loop {
            match self.reader.read::<RuntimeEvent>().await {
                Some(Ok(ev)) => return Some(ev),
                Some(Err(err)) if err.is_recoverable() => continue,
                Some(Err(_)) | None => return None,
            }
        }
    }
}

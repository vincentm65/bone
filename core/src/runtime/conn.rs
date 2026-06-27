//! The frontend ↔ core transport seam.
//!
//! A frontend renders a turn purely by pulling [`RuntimeEvent`]s and pushing
//! [`RuntimeCommand`]s through a [`RuntimeConn`]. Two implementations carry the
//! exact same protocol:
//!
//! - [`LocalConn`] — the runtime runs in-process: it owns the in-flight
//!   `Driver` future and polls it on the frontend's own task, so there is no
//!   spawn and the Lua VM is never touched by the render path while a tool runs.
//!   This is what `bone` uses with no `--listen`.
//! - `SocketConn` (Phase 3) — the runtime is a separate `bone serve` daemon and
//!   the same events/commands cross a socket via the JSONL `rpc::codec`.
//!
//! The TUI is therefore a *client* either way; "local" just means the server
//! lives in the same process. This is the Neovim model — even the built-in UI
//! talks the protocol.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;

use crate::rpc::codec;
use crate::runtime::driver::{Driver, DriverOutcome};
use crate::runtime::{ApprovalReplyRegistry, KeyReplyRegistry, RuntimeCommand, RuntimeEvent};

/// The transport a frontend renders a turn through. Frontend-neutral: the same
/// trait backs the in-process [`LocalConn`] and the socket client.
pub trait RuntimeConn {
    /// Push a command to the runtime (submit a prompt, answer an approval/key
    /// request, cancel the turn).
    fn send(&mut self, cmd: RuntimeCommand);

    /// Pull the next event. `None` means the current turn has fully drained
    /// (the runtime is idle) — the frontend loop should stop pumping events.
    ///
    /// Not `Send`: the in-process [`LocalConn`] holds the Driver future, which
    /// owns the Lua VM handle and is polled on the frontend's own task (never
    /// spawned), so it need not cross threads.
    fn next_event(&mut self) -> impl Future<Output = Option<RuntimeEvent>>;
}

/// In-process runtime connection: owns the `Driver` future and drives it on the
/// frontend's task.
///
/// `send(SubmitPrompt)` starts the turn; `next_event()` interleaves the Driver's
/// streamed events with the future's completion. When the future finishes its
/// [`DriverOutcome`] is stashed for the frontend to reabsorb via
/// [`take_outcome`]. Approval/key replies resolve the shared registries the
/// turn's `ChannelApprovalGate` / `ctx.ui.key` block on; `Cancel` flips the flag
/// the Driver and its tools observe.
///
/// [`take_outcome`]: LocalConn::take_outcome
pub struct LocalConn {
    events_rx: mpsc::UnboundedReceiver<RuntimeEvent>,
    /// The Driver awaiting a `SubmitPrompt` to start; consumed into `run_fut`.
    driver: Option<Driver>,
    /// The in-flight turn future; `None` before submit and after completion.
    run_fut: Option<Pin<Box<dyn Future<Output = DriverOutcome> + Send>>>,
    /// The completed turn's reclaimable state, awaiting `take_outcome`.
    outcome: Option<DriverOutcome>,
    cancel: Arc<AtomicBool>,
    approval_registry: ApprovalReplyRegistry,
    key_registry: KeyReplyRegistry,
}

impl LocalConn {
    /// Build a connection around a turn's `Driver` and its event stream. The
    /// `cancel` flag and the two registries must be the same instances wired
    /// into the `Driver`/gate so `send` reaches the running turn.
    pub fn new(
        events_rx: mpsc::UnboundedReceiver<RuntimeEvent>,
        driver: Driver,
        cancel: Arc<AtomicBool>,
        approval_registry: ApprovalReplyRegistry,
        key_registry: KeyReplyRegistry,
    ) -> Self {
        Self {
            events_rx,
            driver: Some(driver),
            run_fut: None,
            outcome: None,
            cancel,
            approval_registry,
            key_registry,
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
            RuntimeCommand::Cancel => self.cancel.store(true, Ordering::Relaxed),
            // Daemon-level commands are not part of a single in-process turn.
            RuntimeCommand::RunCommand { .. }
            | RuntimeCommand::NewConversation
            | RuntimeCommand::LoadConversation { .. }
            | RuntimeCommand::ClearConversation
            | RuntimeCommand::SwitchProvider { .. }
            | RuntimeCommand::ReplaceConversation { .. }
            | RuntimeCommand::SetApprovalMode { .. }
            | RuntimeCommand::AppendMessage { .. }
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
    _writer: tokio::task::JoinHandle<()>,
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
            _writer: writer,
        }
    }

    /// A cloneable handle for queuing commands without borrowing the connection
    /// — lets a client push prompts/replies from a `select!` arm while another
    /// arm holds `&mut self` in [`next_event`](RuntimeConn::next_event).
    pub fn command_sender(&self) -> mpsc::UnboundedSender<RuntimeCommand> {
        self.cmd_tx.clone()
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
                // Skip a malformed frame; the connection is still healthy.
                Some(Err(_)) => continue,
                None => return None, // connection closed
            }
        }
    }
}

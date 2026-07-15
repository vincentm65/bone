//! The frontend boundary protocol: `RuntimeEvent` (core → frontend) and
//! `RuntimeCommand` (frontend → core).
//!
//! The plain serde types flow over both an in-process channel and the RPC
//! transport. They also serve as the headless run event type; the JSONL path
//! prints a stable projection of these events for `bone run --events`.
//!
//! Key requests are the one piece that cannot be a pure value: `KeyRequest`
//! carries a live `oneshot::Sender`. The [`KeyReplyRegistry`] splits that into
//! an id sent to the frontend plus an id-keyed table of pending reply channels.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, oneshot};

use crate::pane_content::{KeyEvent, KeyRequest};
use crate::runtime::timer::WorkTimer;
use crate::tools::{ApprovalGate, CallOutcome, ToolCall, decide_call};

// Re-export wire-format types from protocol.
pub use bone_protocol::{CommandAction, RuntimeCommand, RuntimeEvent, SessionSnapshot};

/// Routes key replies from the frontend back to blocked callers.
#[derive(Clone, Default)]
pub struct KeyReplyRegistry {
    inner: Arc<Mutex<RegistryInner>>,
    timer: Arc<Mutex<Option<WorkTimer>>>,
}

#[derive(Default)]
struct RegistryInner {
    next_id: u64,
    pending: HashMap<u64, oneshot::Sender<KeyEvent>>,
}

impl KeyReplyRegistry {
    pub fn set_timer(&self, timer: Option<WorkTimer>) {
        *self.timer.lock().unwrap_or_else(|e| e.into_inner()) = timer;
    }

    pub fn new() -> Self {
        Self::default()
    }

    /// Take ownership of `req`'s reply channel and assign it an id.
    pub fn register(&self, req: KeyRequest) -> u64 {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let id = g.next_id;
        g.next_id = g.next_id.wrapping_add(1);
        let was_empty = g.pending.is_empty();
        g.pending.insert(id, req.reply);
        if was_empty {
            if let Some(timer) = self
                .timer
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
            {
                timer.pause();
            }
        }
        id
    }

    /// Deliver `key` to the caller blocked on request `id`.
    pub fn resolve(&self, id: u64, key: KeyEvent) -> bool {
        let (sender, now_empty) = {
            let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let sender = g.pending.remove(&id);
            let now_empty = sender.is_some() && g.pending.is_empty();
            (sender, now_empty)
        };
        match sender {
            Some(tx) => {
                if now_empty {
                    if let Some(timer) = self
                        .timer
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .as_ref()
                    {
                        timer.resume();
                    }
                }
                let _ = tx.send(key);
                true
            }
            None => false,
        }
    }

    /// Number of keys still awaiting a reply (for diagnostics/tests).
    pub fn pending_count(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending
            .len()
    }
}

/// Routes tool-approval decisions from the frontend back to the blocked gate.
///
/// The serializable analogue of [`KeyReplyRegistry`]: an interactive approval
/// can't be a pure value (the gate blocks on a live `oneshot`), so the gate
/// registers its reply channel here, emits a serializable
/// [`RuntimeEvent::ApprovalRequest`] carrying only the assigned `id`, and the
/// frontend answers with [`RuntimeCommand::ApprovalReply`], which `resolve`s the
/// `id` back to the waiting gate. This is what lets approval flow over RPC.
#[derive(Clone, Default)]
pub struct ApprovalReplyRegistry {
    inner: Arc<Mutex<ApprovalRegistryInner>>,
}

#[derive(Default)]
struct ApprovalRegistryInner {
    next_id: u64,
    pending: HashMap<u64, oneshot::Sender<CallOutcome>>,
}

impl ApprovalReplyRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take ownership of a reply channel and assign it an id.
    pub fn register(&self, reply: oneshot::Sender<CallOutcome>) -> u64 {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let id = g.next_id;
        g.next_id = g.next_id.wrapping_add(1);
        g.pending.insert(id, reply);
        id
    }

    /// Deliver `outcome` to the gate blocked on request `id`.
    pub fn resolve(&self, id: u64, outcome: CallOutcome) -> bool {
        let sender = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending
            .remove(&id);
        match sender {
            Some(tx) => tx.send(outcome).is_ok(),
            None => false,
        }
    }

    /// Number of approvals still awaiting a reply (for diagnostics/tests).
    pub fn pending_count(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending
            .len()
    }
}

/// Resolves tool-call approval by asking a frontend over the runtime protocol.
///
/// Implements [`ApprovalGate`] by registering its reply channel in an
/// [`ApprovalReplyRegistry`], emitting a [`RuntimeEvent::ApprovalRequest`] on the
/// frontend event stream, and awaiting the frontend's [`CallOutcome`]. If the
/// event stream is gone or the reply is dropped, it falls back to the
/// non-interactive [`decide_call`] — so a detached frontend can never wedge the
/// loop. Because it emits a plain `RuntimeEvent` and is answered by a plain
/// `RuntimeCommand`, the same gate serves the in-process TUI and a remote client.
#[derive(Clone)]
pub struct ChannelApprovalGate {
    events: mpsc::UnboundedSender<RuntimeEvent>,
    registry: ApprovalReplyRegistry,
    timer: Option<WorkTimer>,
    working_dir: Option<std::path::PathBuf>,
}

impl ChannelApprovalGate {
    pub fn new(
        events: mpsc::UnboundedSender<RuntimeEvent>,
        registry: ApprovalReplyRegistry,
        timer: Option<WorkTimer>,
        working_dir: Option<std::path::PathBuf>,
    ) -> Self {
        Self {
            events,
            registry,
            timer,
            working_dir,
        }
    }
}

#[async_trait::async_trait]
impl ApprovalGate for ChannelApprovalGate {
    async fn decide(
        &self,
        blocked: Option<String>,
        auto_allows: bool,
        call: &ToolCall,
    ) -> CallOutcome {
        let (reply_tx, reply_rx) = oneshot::channel();
        let id = self.registry.register(reply_tx);
        let preview = if call.name == "edit_file" {
            crate::tools::edit_file::preview_edit_file(
                &call.name,
                call.arguments.clone(),
                self.working_dir.as_deref(),
            )
            .await
            .ok()
            .map(|preview| preview.diff)
        } else {
            None
        };
        let event = RuntimeEvent::ApprovalRequest {
            id,
            call_id: call.id.clone(),
            name: call.name.clone(),
            summary: crate::agent::summarize_call_args(call),
            arguments: call.arguments.clone(),
            blocked: blocked.clone(),
            auto_allows,
            preview,
        };
        if self.events.send(event).is_err() {
            // Frontend detached: fall back without wedging the loop.
            return decide_call(blocked, auto_allows);
        }
        if let Some(timer) = &self.timer {
            timer.pause();
        }
        let outcome = match reply_rx.await {
            Ok(outcome) => outcome,
            // Frontend dropped the reply (detached/cancelled): fall back.
            Err(_) => decide_call(blocked, auto_allows),
        };
        if let Some(timer) = &self.timer {
            timer.resume();
        }
        outcome
    }
}

#[cfg(test)]
#[path = "event_tests.rs"]
mod event_tests;

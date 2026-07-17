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

/// Shared id allocation and pending-sender lifecycle for interactive replies.
struct ReplyRegistry<T> {
    inner: Arc<Mutex<RegistryInner<T>>>,
}

struct RegistryInner<T> {
    next_id: u64,
    pending: HashMap<u64, oneshot::Sender<T>>,
}

impl<T> Default for ReplyRegistry<T> {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(RegistryInner {
                next_id: 0,
                pending: HashMap::new(),
            })),
        }
    }
}

impl<T> Clone for ReplyRegistry<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T> ReplyRegistry<T> {
    fn register(&self, reply: oneshot::Sender<T>) -> (u64, bool) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let id = inner.next_id;
        inner.next_id = inner.next_id.wrapping_add(1);
        let was_empty = inner.pending.is_empty();
        inner.pending.insert(id, reply);
        (id, was_empty)
    }

    fn remove(&self, id: u64) -> (Option<oneshot::Sender<T>>, bool) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let sender = inner.pending.remove(&id);
        let now_empty = sender.is_some() && inner.pending.is_empty();
        (sender, now_empty)
    }

    fn drain(&self) -> Vec<oneshot::Sender<T>> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending
            .drain()
            .map(|(_, sender)| sender)
            .collect()
    }

    fn pending_count(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending
            .len()
    }
}

/// Routes key replies from the frontend back to blocked callers.
#[derive(Clone, Default)]
pub struct KeyReplyRegistry {
    replies: ReplyRegistry<KeyEvent>,
    timer: Arc<Mutex<Option<WorkTimer>>>,
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
        let (id, was_empty) = self.replies.register(req.reply);
        if was_empty {
            self.pause_timer();
        }
        id
    }

    /// Deliver `key` to the caller blocked on request `id`.
    pub fn resolve(&self, id: u64, key: KeyEvent) -> bool {
        let (sender, now_empty) = self.replies.remove(id);
        if now_empty {
            self.resume_timer();
        }
        sender.is_some_and(|tx| tx.send(key).is_ok())
    }

    /// Remove an abandoned request, dropping its reply sender.
    pub fn remove(&self, id: u64) -> bool {
        let (sender, now_empty) = self.replies.remove(id);
        if now_empty {
            self.resume_timer();
        }
        sender.is_some()
    }

    /// Drop every pending key request, unblocking cancelled tools.
    pub fn cancel_all(&self) {
        if !self.replies.drain().is_empty() {
            self.resume_timer();
        }
    }

    /// Number of keys still awaiting a reply (for diagnostics/tests).
    pub fn pending_count(&self) -> usize {
        self.replies.pending_count()
    }

    fn pause_timer(&self) {
        if let Some(timer) = self
            .timer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            timer.pause();
        }
    }

    fn resume_timer(&self) {
        if let Some(timer) = self
            .timer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            timer.resume();
        }
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
    replies: ReplyRegistry<CallOutcome>,
}

impl ApprovalReplyRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take ownership of a reply channel and assign it an id.
    pub fn register(&self, reply: oneshot::Sender<CallOutcome>) -> u64 {
        self.replies.register(reply).0
    }

    /// Deliver `outcome` to the gate blocked on request `id`.
    pub fn resolve(&self, id: u64, outcome: CallOutcome) -> bool {
        self.replies
            .remove(id)
            .0
            .is_some_and(|tx| tx.send(outcome).is_ok())
    }

    /// Remove an abandoned request, dropping its reply sender.
    pub fn remove(&self, id: u64) -> bool {
        self.replies.remove(id).0.is_some()
    }

    /// Deny and remove every pending approval so cancellation cannot wedge a turn.
    pub fn cancel_all(&self) {
        for sender in self.replies.drain() {
            let _ = sender.send(CallOutcome::Denied);
        }
    }

    /// Number of approvals still awaiting a reply (for diagnostics/tests).
    pub fn pending_count(&self) -> usize {
        self.replies.pending_count()
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
            // Frontend detached: unregister before falling back.
            self.registry.remove(id);
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

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
use crate::tools::{ApprovalGate, CallOutcome, ToolCall, decide_call};

// Re-export wire-format types from protocol.
pub use bone_protocol::{CommandAction, RuntimeCommand, RuntimeEvent, SessionSnapshot};

/// Routes key replies from the frontend back to blocked callers.
#[derive(Clone, Default)]
pub struct KeyReplyRegistry {
    inner: Arc<Mutex<RegistryInner>>,
}

#[derive(Default)]
struct RegistryInner {
    next_id: u64,
    pending: HashMap<u64, oneshot::Sender<KeyEvent>>,
}

impl KeyReplyRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take ownership of `req`'s reply channel and assign it an id.
    pub fn register(&self, req: KeyRequest) -> u64 {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let id = g.next_id;
        g.next_id = g.next_id.wrapping_add(1);
        g.pending.insert(id, req.reply);
        id
    }

    /// Deliver `key` to the caller blocked on request `id`.
    pub fn resolve(&self, id: u64, key: KeyEvent) -> bool {
        let sender = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending
            .remove(&id);
        match sender {
            Some(tx) => {
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
}

impl ChannelApprovalGate {
    pub fn new(
        events: mpsc::UnboundedSender<RuntimeEvent>,
        registry: ApprovalReplyRegistry,
    ) -> Self {
        Self { events, registry }
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
        let event = RuntimeEvent::ApprovalRequest {
            id,
            call_id: call.id.clone(),
            name: call.name.clone(),
            summary: crate::agent::summarize_call_args(call),
            arguments: call.arguments.clone(),
            blocked: blocked.clone(),
            auto_allows,
        };
        if self.events.send(event).is_err() {
            // Frontend detached: fall back without wedging the loop.
            return decide_call(blocked, auto_allows);
        }
        match reply_rx.await {
            Ok(outcome) => outcome,
            // Frontend dropped the reply (detached/cancelled): fall back.
            Err(_) => decide_call(blocked, auto_allows),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn key_reply_registry_routes_reply_by_id() {
        let registry = KeyReplyRegistry::new();
        let (tx, rx) = oneshot::channel();
        let id = registry.register(KeyRequest { reply: tx });
        assert_eq!(registry.pending_count(), 1);

        // A reply for a wrong id does nothing.
        let key = KeyEvent {
            code: "Enter".into(),
            char: None,
            ctrl: false,
            alt: false,
            shift: false,
        };
        assert!(!registry.resolve(id.wrapping_add(99), key.clone()));
        assert_eq!(registry.pending_count(), 1);

        // The correct id delivers the value and clears the pending entry.
        assert!(registry.resolve(id, key.clone()));
        assert_eq!(registry.pending_count(), 0);

        let got = rx.await.expect("reply delivered");
        assert_eq!(got, key);
    }
}

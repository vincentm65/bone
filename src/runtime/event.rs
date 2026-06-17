//! The frontend boundary protocol: `RuntimeEvent` (core → frontend) and
//! `RuntimeCommand` (frontend → core).
//!
//! These are plain serde types so the exact same messages flow over an
//! in-process channel today and over an RPC transport (Phase 5) tomorrow. They
//! also serve as the headless run event type; the JSONL path prints a stable
//! projection of these events for `bone run --events`.
//!
//! Key requests are the one piece that cannot be a pure value: `KeyRequest`
//! carries a live `oneshot::Sender`. The [`KeyReplyRegistry`] splits that into
//! an id sent to the frontend plus an id-keyed table of pending reply channels.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

use crate::pane_content::{KeyEvent, KeyRequest, PaneContent};
use crate::tools::{ApprovalGate, CallOutcome, ToolCall, decide_call};

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

/// A request for the frontend to approve (or decline) a tool call.
///
/// The interactive analogue of [`crate::tools::AutoApprovalGate`]: the `Driver`
/// can't call back into a `&mut App`+terminal, so a [`ChannelApprovalGate`]
/// sends this request to the frontend's event loop and awaits the decision on
/// `reply`. `auto_allows` lets the frontend skip prompting when the approval
/// mode already permits the call (it just replies `Approve`); `blocked` carries
/// an extension-hook veto. Not serializable — it owns a live `oneshot` sender;
/// the wire/RPC path uses serializable runtime events instead.
pub struct ApprovalRequest {
    pub call: ToolCall,
    pub blocked: Option<String>,
    pub auto_allows: bool,
    pub reply: oneshot::Sender<CallOutcome>,
}

/// Resolves tool-call approval by asking a frontend over a channel.
///
/// Implements [`ApprovalGate`] by sending an [`ApprovalRequest`] and awaiting
/// the frontend's [`CallOutcome`]. If the channel is gone or the reply is
/// dropped, it falls back to the non-interactive [`decide_call`] — so a detached
/// frontend can never wedge the loop.
#[derive(Clone)]
pub struct ChannelApprovalGate {
    tx: mpsc::UnboundedSender<ApprovalRequest>,
}

impl ChannelApprovalGate {
    pub fn new(tx: mpsc::UnboundedSender<ApprovalRequest>) -> Self {
        Self { tx }
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
        let request = ApprovalRequest {
            call: call.clone(),
            blocked: blocked.clone(),
            auto_allows,
            reply: reply_tx,
        };
        if self.tx.send(request).is_err() {
            return decide_call(blocked, auto_allows);
        }
        match reply_rx.await {
            Ok(outcome) => outcome,
            // Frontend dropped the reply (detached/cancelled): fall back.
            Err(_) => decide_call(blocked, auto_allows),
        }
    }
}

/// Core → frontend. Everything a frontend needs to render a turn.
///
/// Externally tagged with `snake_case` variant names, so the JSON form is
/// stable and self-describing (e.g. `{"text_delta":{"text":"hi"}}`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeEvent {
    /// A turn started.
    Started {
        approval: String,
        task: String,
        model: String,
    },
    /// Human-readable status line ("thinking", "running shell: …").
    Status { message: String },
    /// Incremental assistant text.
    TextDelta { text: String },
    /// Incremental reasoning/thinking text.
    ReasoningDelta { text: String },
    /// The model requested a tool call.
    ToolCall {
        id: String,
        name: String,
        summary: String,
        #[serde(default)]
        arguments: serde_json::Value,
    },
    /// A tool finished.
    ToolResult {
        name: String,
        call_id: String,
        is_error: bool,
        #[serde(default)]
        content: String,
    },
    /// Token accounting update. `context_length` is the current prompt size
    /// (the `curr` metric) after this request — what compaction watches.
    TokenUsage {
        sent: u64,
        received: u64,
        context_length: u64,
    },
    /// A pane upsert/remove. In Phase 4 this becomes part of a richer
    /// `ViewUpdate(ViewDiff)`; for now a pane is the unit of view change.
    Pane { pane: PaneContent },
    /// The runtime is requesting the next terminal key.
    KeyRequest { id: u64 },
    /// The turn finished with a final assistant message.
    Finished { content: String },
    /// The turn failed.
    Failed { message: String },
}

/// Frontend → core. Everything a frontend can ask the runtime to do.
///
/// `Key` carries a frontend-neutral string descriptor (e.g. `"ctrl+c"`,
/// `"enter"`) rather than a crossterm type, so core stays terminal-agnostic.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeCommand {
    /// Submit a user turn.
    SubmitPrompt { text: String },
    /// Approve or deny a tool call awaiting interactive approval.
    ApprovalReply { call_id: String, approved: bool },
    /// Answer an outstanding [`RuntimeEvent::KeyRequest`] request.
    KeyReply { id: u64, key: KeyEvent },
    /// Cancel the in-flight turn.
    Cancel,
    /// A key press, as a frontend-neutral descriptor.
    Key { key: String },
    /// Terminal/viewport resize.
    Resize { cols: u16, rows: u16 },
    /// Run a registered (slash) command.
    RunCommand { name: String, input: String },
    /// Invoke a Lua runtime API method (the RPC entry to `bone.api`, Phase 6).
    ApiCall {
        method: String,
        params: serde_json::Value,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Re-serialize a value and compare JSON, so we test round-trip without
    /// requiring `PartialEq` on `PaneContent`.
    fn roundtrip_event(ev: &RuntimeEvent) -> RuntimeEvent {
        let s = serde_json::to_string(ev).expect("serialize");
        serde_json::from_str(&s).expect("deserialize")
    }

    fn json_of(ev: &RuntimeEvent) -> serde_json::Value {
        serde_json::to_value(ev).expect("to_value")
    }

    #[test]
    fn every_runtime_event_variant_round_trips() {
        let variants = vec![
            RuntimeEvent::Started {
                approval: "safe".into(),
                task: "do it".into(),
                model: "m".into(),
            },
            RuntimeEvent::Status {
                message: "thinking".into(),
            },
            RuntimeEvent::TextDelta { text: "hi".into() },
            RuntimeEvent::ReasoningDelta { text: "hmm".into() },
            RuntimeEvent::ToolCall {
                id: "c1".into(),
                name: "shell".into(),
                summary: "ls".into(),
                arguments: json!({ "command": "ls" }),
            },
            RuntimeEvent::ToolResult {
                name: "shell".into(),
                call_id: "c1".into(),
                is_error: false,
                content: "files".into(),
            },
            RuntimeEvent::TokenUsage {
                sent: 10,
                received: 2,
                context_length: 8,
            },
            RuntimeEvent::Pane {
                pane: PaneContent {
                    source: "s".into(),
                    title: "t".into(),
                    lines: vec![],
                    visible_rows: 8,
                    scroll: 0,
                },
            },
            RuntimeEvent::KeyRequest { id: 7 },
            RuntimeEvent::Finished {
                content: "done".into(),
            },
            RuntimeEvent::Failed {
                message: "boom".into(),
            },
        ];
        for ev in &variants {
            assert_eq!(
                json_of(ev),
                json_of(&roundtrip_event(ev)),
                "round-trip {ev:?}"
            );
        }
    }

    #[test]
    fn every_runtime_command_variant_round_trips() {
        let cmds = vec![
            RuntimeCommand::SubmitPrompt { text: "hi".into() },
            RuntimeCommand::ApprovalReply {
                call_id: "c1".into(),
                approved: true,
            },
            RuntimeCommand::KeyReply {
                id: 7,
                key: KeyEvent {
                    code: "Enter".into(),
                    char: None,
                    ctrl: false,
                    alt: false,
                    shift: false,
                },
            },
            RuntimeCommand::Cancel,
            RuntimeCommand::Key {
                key: "ctrl+c".into(),
            },
            RuntimeCommand::Resize { cols: 80, rows: 24 },
            RuntimeCommand::RunCommand {
                name: "usage".into(),
                input: "".into(),
            },
            RuntimeCommand::ApiCall {
                method: "ui.open_float".into(),
                params: json!({"title": "x"}),
            },
        ];
        for cmd in &cmds {
            let s = serde_json::to_string(cmd).expect("serialize");
            let back: RuntimeCommand = serde_json::from_str(&s).expect("deserialize");
            assert_eq!(
                serde_json::to_value(cmd).unwrap(),
                serde_json::to_value(&back).unwrap(),
                "round-trip {cmd:?}"
            );
        }
    }

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

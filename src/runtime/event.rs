//! The frontend boundary protocol: `RuntimeEvent` (core → frontend) and
//! `RuntimeCommand` (frontend → core).
//!
//! These are plain serde types so the exact same messages flow over an
//! in-process channel today and over an RPC transport (Phase 5) tomorrow. They
//! also serve as the headless run event type; the JSONL path prints a stable
//! projection of these events for `bone run --events`.
//!
//! Interaction is the one piece that cannot be a pure value: `InteractRequest`
//! carries a live `oneshot::Sender`. The [`ReplyRegistry`] splits that into a
//! serializable [`InteractSpec`] (sent to the frontend) plus an id-keyed table
//! of pending reply channels — so a reply arriving as a
//! [`RuntimeCommand::InteractReply`] routes back to the blocked caller by id.
//! This is exactly the indirection a remote transport needs.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

use crate::pane_content::{InteractRequest, InteractionMode, PaneContent};
use crate::tools::{ApprovalGate, CallOutcome, ToolCall, decide_call};

/// Serializable form of an interaction request.
///
/// The wire-safe projection of [`InteractRequest`] (which additionally holds a
/// non-serializable `oneshot::Sender`). `id` lets the eventual reply route back
/// to the blocked caller via [`ReplyRegistry::resolve`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractSpec {
    pub id: u64,
    pub question: String,
    pub mode: InteractionMode,
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub default_selected: usize,
    #[serde(default)]
    pub allow_custom: bool,
}

/// Routes interaction replies from the frontend back to blocked callers.
///
/// When the Driver receives an `InteractRequest` (which owns a `oneshot`
/// sender), it [`register`](Self::register)s it: the registry stores the sender
/// under a fresh id and hands back an [`InteractSpec`] to ship to the frontend.
/// When the frontend answers with [`RuntimeCommand::InteractReply`], the Driver
/// calls [`resolve`](Self::resolve) to deliver the value to the waiting sender.
#[derive(Clone, Default)]
pub struct ReplyRegistry {
    inner: Arc<Mutex<RegistryInner>>,
}

#[derive(Default)]
struct RegistryInner {
    next_id: u64,
    pending: HashMap<u64, oneshot::Sender<serde_json::Value>>,
}

impl ReplyRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take ownership of `req`'s reply channel, assign it an id, and return the
    /// serializable spec to send to the frontend.
    pub fn register(&self, req: InteractRequest) -> InteractSpec {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let id = g.next_id;
        g.next_id = g.next_id.wrapping_add(1);
        g.pending.insert(id, req.reply);
        InteractSpec {
            id,
            question: req.question,
            mode: req.mode,
            options: req.options,
            default_selected: req.default_selected,
            allow_custom: req.allow_custom,
        }
    }

    /// Deliver `value` to the caller blocked on interaction `id`.
    ///
    /// Returns `true` if a pending request was found and the value was sent
    /// (the receiver may still have been dropped, in which case the send is a
    /// no-op but we still report the id as resolved).
    pub fn resolve(&self, id: u64, value: serde_json::Value) -> bool {
        let sender = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending
            .remove(&id);
        match sender {
            Some(tx) => {
                let _ = tx.send(value);
                true
            }
            None => false,
        }
    }

    /// Number of interactions still awaiting a reply (for diagnostics/tests).
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
/// the wire/RPC path uses `ReplyRegistry` + `RuntimeEvent::Approval` instead.
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
    /// The runtime is requesting user interaction; the frontend should present
    /// it and reply with [`RuntimeCommand::InteractReply`] carrying the `id`.
    Interact { spec: InteractSpec },
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
    /// Answer an outstanding [`RuntimeEvent::Interact`] request.
    InteractReply { id: u64, value: serde_json::Value },
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
            RuntimeEvent::Interact {
                spec: InteractSpec {
                    id: 7,
                    question: "pick".into(),
                    mode: InteractionMode::SingleSelect,
                    options: vec!["a".into(), "b".into()],
                    default_selected: 0,
                    allow_custom: false,
                },
            },
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
            RuntimeCommand::InteractReply {
                id: 7,
                value: json!({"value": "a"}),
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
    async fn reply_registry_routes_reply_by_id() {
        let registry = ReplyRegistry::new();
        let (tx, rx) = oneshot::channel();
        let spec = registry.register(InteractRequest {
            question: "pick one".into(),
            mode: InteractionMode::SingleSelect,
            options: vec!["a".into(), "b".into()],
            default_selected: 0,
            allow_custom: false,
            reply: tx,
        });
        assert_eq!(registry.pending_count(), 1);

        // A reply for a wrong id does nothing.
        assert!(!registry.resolve(spec.id.wrapping_add(99), json!("x")));
        assert_eq!(registry.pending_count(), 1);

        // The correct id delivers the value and clears the pending entry.
        assert!(registry.resolve(spec.id, json!({"value": "b"})));
        assert_eq!(registry.pending_count(), 0);

        let got = rx.await.expect("reply delivered");
        assert_eq!(got, json!({"value": "b"}));
    }
}

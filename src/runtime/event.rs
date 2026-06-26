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

use crate::pane_content::{KeyEvent, KeyRequest};
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
    /// Human-readable status line ("thinking", "running shell: …"). Transient:
    /// frontends may show it in the status/spinner area and let it be replaced.
    Status { message: String },
    /// A persistent notice for the conversation scrollback (e.g. an
    /// auto-compaction announcement). Unlike [`Status`], frontends keep it in
    /// the transcript rather than treating it as ephemeral. Emitted by
    /// `ctx.ui.notice` so Lua can surface a message without the host having to
    /// guess from the text which status lines are worth keeping.
    Notice { message: String },
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
    /// The runtime is requesting the next terminal key.
    KeyRequest { id: u64 },
    /// The runtime is asking the frontend to approve (or decline) a tool call.
    /// `id` routes the eventual [`RuntimeCommand::ApprovalReply`] back to the
    /// blocked gate. `auto_allows` is `true` when the approval mode already
    /// permits the call, letting a frontend approve without prompting; `blocked`
    /// carries an extension-hook veto reason when present.
    ApprovalRequest {
        id: u64,
        call_id: String,
        name: String,
        summary: String,
        #[serde(default)]
        arguments: serde_json::Value,
        #[serde(default)]
        blocked: Option<String>,
        auto_allows: bool,
    },
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
    /// Answer an outstanding [`RuntimeEvent::ApprovalRequest`]. `id` routes the
    /// `outcome` back to the blocked gate (approve / deny / block-with-advice).
    ApprovalReply { id: u64, outcome: CallOutcome },
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
            RuntimeEvent::KeyRequest { id: 7 },
            RuntimeEvent::ApprovalRequest {
                id: 3,
                call_id: "c1".into(),
                name: "shell".into(),
                summary: "shell: ls".into(),
                arguments: json!({ "command": "ls" }),
                blocked: None,
                auto_allows: false,
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
                id: 3,
                outcome: CallOutcome::Blocked("user advice".into()),
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

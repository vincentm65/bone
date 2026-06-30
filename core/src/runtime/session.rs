//! `RuntimeSession` — the persistent owner of a conversation's turn-truth.
//!
//! Holds the state that survives *across* turns and is authoritative regardless
//! of which frontend is attached: the tool handler (and its cross-round state
//! map), the running transcript, cumulative token stats, and the SQLite session
//! persistence. A turn is run by building a [`Driver`] from this state
//! ([`build_driver`]) and folding its [`DriverOutcome`] back in
//! ([`apply_outcome`]) — which also persists the turn.
//!
//! This is what makes every frontend a *client*: the daemon (`run_daemon`)
//! owns the `RuntimeSession` — both the in-process `bone` daemon (on the same
//! `LocalSet` as the TUI) and the standalone `bone serve`. The TUI never holds
//! one; it pushes commands and renders the event stream while the session keeps
//! the truth, so a frontend never needs the `DriverOutcome`.
//!
//! `llm` and `extensions` are *not* owned here: the daemon that runs the turns
//! owns them and passes them into [`build_driver`] per turn. (The in-process
//! TUI also keeps a provider handle for display strings, and shares the daemon's
//! Lua VM, but the session itself does not own either.)
//!
//! Part of core — no `crate::ui`, compiles ratatui-free.
//!
//! [`build_driver`]: RuntimeSession::build_driver
//! [`apply_outcome`]: RuntimeSession::apply_outcome

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tokio::sync::mpsc::UnboundedSender;

use crate::chat::build_chat_history;
use crate::ext::ExtensionManager;
use crate::llm::provider::LlmProvider;
use crate::llm::{ChatMessage, TokenStats};
use crate::runtime::driver::{Driver, DriverOutcome};
use crate::runtime::{KeyReplyRegistry, RuntimeEvent};
use crate::session_db::SessionDb;
use crate::session_sink::SessionSink;
use crate::tools::registry::ToolHandler;
use crate::tools::{ApprovalGate, SharedApprovalMode};
use bone_protocol::SessionSnapshot;

/// Owns the agent turn-truth for one conversation.
pub struct RuntimeSession {
    /// Tool handler; its `state_map` carries cross-round stateful-tool state.
    pub tools: ToolHandler,
    /// The full conversation transcript (what the next turn's history is built
    /// from, and what `/history` renders).
    pub transcript: Vec<ChatMessage>,
    /// Cumulative token accounting across the conversation.
    pub token_stats: TokenStats,
    /// SQLite persistence (None until opened, or if opening failed).
    pub session_db: Option<SessionDb>,
    /// Active conversation row id in `session_db`.
    pub conversation_id: Option<i64>,
    /// Monotonic message sequence within the conversation.
    pub session_seq: i64,
}

impl RuntimeSession {
    /// A fresh session with the given tool handler and no DB yet.
    pub fn new(tools: ToolHandler) -> Self {
        Self {
            tools,
            transcript: Vec::new(),
            token_stats: TokenStats::new(),
            session_db: None,
            conversation_id: None,
            session_seq: 0,
        }
    }

    /// Open the session database and make a conversation active for `llm`'s
    /// provider/model. Idempotent; returns a human-readable warning on failure.
    ///
    /// Boot resumes the **most recent** conversation in place rather than minting
    /// a fresh one every launch: a non-empty conversation has its transcript
    /// reloaded (so a restarted TUI / `bone serve` picks up where it left off
    /// instead of opening an empty chat), a trailing empty conversation is
    /// recycled, and only a truly empty database mints a new row. This is what
    /// makes the conversation survive a runtime restart — the data was always
    /// persisted, it just wasn't being reattached.
    pub fn init_db(&mut self, llm: &dyn LlmProvider) -> Option<String> {
        if self.session_db.is_some() {
            return None;
        }
        let db_path = crate::session_db::db_path();
        let db = match SessionDb::open(&db_path) {
            Ok(db) => db,
            Err(err) => return Some(format!("warning: failed to open session database: {err}")),
        };
        match db.latest_conversation() {
            // Resume the last conversation in place.
            Ok(Some((conv_id, has_messages))) => {
                if has_messages {
                    match db.list_messages(conv_id, 1000) {
                        Ok(rows) => {
                            self.transcript = rows
                                .into_iter()
                                .map(crate::session_db::stored_to_chat_message)
                                .collect();
                        }
                        Err(err) => {
                            return Some(format!("warning: failed to load conversation: {err}"));
                        }
                    }
                }
                let _ = db.reopen_conversation(conv_id);
                self.session_seq = db.max_message_seq(conv_id).unwrap_or(0);
                self.conversation_id = Some(conv_id);
                self.session_db = Some(db);
                self.recompute_context_estimate();
                None
            }
            // Empty database: mint the first conversation.
            Ok(None) => match db.create_conversation(llm.id(), llm.model()) {
                Ok(conv_id) => {
                    self.conversation_id = Some(conv_id);
                    self.session_db = Some(db);
                    None
                }
                Err(err) => Some(format!("warning: failed to create conversation: {err}")),
            },
            Err(err) => Some(format!("warning: failed to read conversations: {err}")),
        }
    }

    /// Open a fresh durable conversation for an independently managed runtime.
    pub fn init_db_new(&mut self, llm: &dyn LlmProvider) -> Option<String> {
        if self.session_db.is_some() {
            return None;
        }
        let db = match SessionDb::open(&crate::session_db::db_path()) {
            Ok(db) => db,
            Err(err) => return Some(format!("warning: failed to open session database: {err}")),
        };
        match db.create_conversation(llm.id(), llm.model()) {
            Ok(conv_id) => {
                self.conversation_id = Some(conv_id);
                self.session_db = Some(db);
                None
            }
            Err(err) => Some(format!("warning: failed to create conversation: {err}")),
        }
    }

    /// Open one existing durable conversation for an independently managed
    /// runtime. This does not end any other conversation: multiple actors may
    /// remain active at the same time.
    pub fn init_db_conversation(
        &mut self,
        _llm: &dyn LlmProvider,
        conversation_id: i64,
    ) -> Option<String> {
        if self.session_db.is_some() {
            return None;
        }
        let db = match SessionDb::open(&crate::session_db::db_path()) {
            Ok(db) => db,
            Err(err) => return Some(format!("warning: failed to open session database: {err}")),
        };
        match db.conversation_exists(conversation_id) {
            Ok(false) => return Some(format!("conversation {conversation_id} does not exist")),
            Err(err) => {
                return Some(format!(
                    "failed to read conversation {conversation_id}: {err}"
                ));
            }
            Ok(true) => {}
        }
        match db.list_messages(conversation_id, 1000) {
            Ok(rows) => {
                self.transcript = rows
                    .into_iter()
                    .map(crate::session_db::stored_to_chat_message)
                    .collect();
            }
            Err(err) => {
                return Some(format!(
                    "failed to load conversation {conversation_id}: {err}"
                ));
            }
        }
        let _ = db.reopen_conversation(conversation_id);
        self.session_seq = db.max_message_seq(conversation_id).unwrap_or(0);
        self.conversation_id = Some(conversation_id);
        self.session_db = Some(db);
        self.recompute_context_estimate();
        None
    }

    /// Re-estimate the prompt context size from the current transcript + tool
    /// definitions so the token meter reflects a resumed conversation before the
    /// next turn refreshes it. Cheap; no-op-safe on an empty transcript.
    fn recompute_context_estimate(&mut self) {
        let history = build_chat_history(&self.transcript, None);
        let tool_defs_json_chars = serde_json::to_value(self.tools.definitions())
            .map(|v| v.to_string().chars().count())
            .unwrap_or(0);
        let prompt_chars = crate::agent::estimate_context_chars(&history, tool_defs_json_chars);
        self.token_stats.set_context_estimate(prompt_chars);
    }

    /// Append one message to the active conversation, allocating the next
    /// sequence number. No-op when no db/conversation is open.
    #[allow(clippy::too_many_arguments)]
    pub fn append_db_message(
        &mut self,
        role: &str,
        content: &str,
        tool_name: Option<&str>,
        call_id: Option<&str>,
        tool_calls_json: Option<&str>,
        images_json: Option<&str>,
    ) {
        let Some(conv_id) = self.conversation_id else {
            return;
        };
        let Some(db) = self.session_db.as_ref() else {
            return;
        };
        self.session_seq += 1;
        db.append_message(
            conv_id,
            role,
            content,
            tool_name,
            call_id,
            tool_calls_json,
            images_json,
            false,
            self.session_seq,
        )
        .ok();
    }

    /// Append a user message (optionally with image attachments) to the DB. The
    /// turn's assistant/tool messages + usage are batched at turn end by
    /// [`apply_outcome`](Self::apply_outcome).
    pub fn append_user_to_db(&mut self, content: &str, images_json: Option<&str>) {
        self.append_db_message("user", content, None, None, None, images_json);
    }

    /// Build a [`Driver`] for one turn from the current session state. The
    /// caller supplies the per-turn wiring (shared `llm`/`extensions`, the
    /// approval gate + mode, the frontend event stream, key registry, cancel
    /// flag, and a session sink). History is rebuilt from the transcript.
    #[allow(clippy::too_many_arguments)]
    pub fn build_driver(
        &self,
        llm: Arc<dyn LlmProvider>,
        extensions: ExtensionManager,
        approval_mode: SharedApprovalMode,
        gate: Arc<dyn ApprovalGate>,
        runtime_events: UnboundedSender<RuntimeEvent>,
        key_registry: KeyReplyRegistry,
        cancel: Arc<AtomicBool>,
        session_sink: Arc<dyn SessionSink>,
    ) -> Driver {
        Driver {
            llm,
            extensions,
            tools: self.tools.clone(),
            session: session_sink,
            gate,
            approval_mode,
            agent_depth: 0,
            activity: None,
            on_token_usage: None,
            events: false,
            event_sender: None,
            runtime_events: Some(runtime_events),
            key_reply_registry: Some(key_registry),
            cancel: Some(cancel),
            history: build_chat_history(&self.transcript, None),
            transcript: self.transcript.clone(),
            token_stats: self.token_stats.clone(),
            system_prompt_override: None,
            conversation_id: self.conversation_id,
        }
    }

    /// Fold a completed turn's [`DriverOutcome`] back into the session: adopt the
    /// authoritative transcript/token-stats/tool-state and persist the turn's new
    /// messages + usage in one transaction. `persist_from` is the transcript
    /// index where this turn's new (assistant/tool) messages began.
    ///
    /// Returns the turn's `result` so the frontend can surface a failure —
    /// persistence and state adoption happen regardless of success.
    pub fn apply_outcome(
        &mut self,
        outcome: DriverOutcome,
        persist_from: usize,
    ) -> Result<crate::agent::AgentResponse, String> {
        let DriverOutcome {
            result,
            tools,
            transcript,
            token_stats,
            usage,
        } = outcome;
        self.transcript = transcript;
        self.token_stats = token_stats;
        self.tools.state_map = tools.state_map;

        // Persist the turn's new messages + usage in one atomic transaction (a
        // single WAL sync) instead of one commit per row.
        let new_msgs: Vec<ChatMessage> = self
            .transcript
            .get(persist_from..)
            .map(<[ChatMessage]>::to_vec)
            .unwrap_or_default();
        if let Some(ref db) = self.session_db
            && let Some(conv_id) = self.conversation_id
            && let Ok(next) = db.append_turn(conv_id, self.session_seq, &new_msgs, &usage)
        {
            self.session_seq = next;
        }
        result
    }

    /// Snapshot the cumulative session state for a frontend. Carries the token
    /// totals, transcript length, conversation id/seq, plus the active provider
    /// id/model (which live on the caller's `llm`, not here). A frontend mirrors
    /// this from [`RuntimeEvent::StateSnapshot`] instead of reading the session
    /// directly — the same value the daemon publishes after each turn.
    pub fn snapshot(&self, provider_id: &str, provider_model: &str) -> SessionSnapshot {
        SessionSnapshot {
            sent: self.token_stats.sent,
            received: self.token_stats.received,
            cached: self.token_stats.cached,
            cost: self.token_stats.cost,
            request_count: self.token_stats.request_count,
            context_length: self.token_stats.context_length,
            transcript_len: self.transcript.len(),
            conversation_id: self.conversation_id,
            session_seq: self.session_seq,
            provider_id: provider_id.to_string(),
            provider_model: provider_model.to_string(),
        }
    }
}

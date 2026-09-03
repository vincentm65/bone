//! Session persistence sink — injectable seam for conversation/usage recording.
//!
//! `SessionSink` is the object-safe trait for the four operations the agent
//! loop performs (`conv_id`, `append_chat_message`, `record_usage`, `end`).
//! `AgentRequest` accepts `Option<Arc<dyn SessionSink>>`: when present it is
//! used verbatim. Without one, top-level headless runs construct a real
//! [`SessionWriter`]; delegated agents use either [`UsageOnlySessionSink`]
//! (parent conversation id known — nested tokens show in `/stats`) or
//! [`NullSessionSink`] (no parent / no DB).
//!
//! [`NullSessionSink`] is a no-op (`conv_id == None`), for tests and drivers
//! that need zero side-effects and zero file I/O.
//!
//! Note: the interactive daemon path owns transcript via `RuntimeSession`;
//! this sink is the headless / sub-agent write path. Do not invent a third.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::llm::ChatMessage;
use crate::session_db::{SessionDb, db_path};

/// Sink for persisting agent conversation turns and token usage.
///
/// All methods take `&self`; the concrete `SessionWriter` holds a single
/// `Mutex`-guarded connection (write methods lock, mutate, and return `()`),
/// so the trait is object-safe and shareable via `Arc<dyn SessionSink>`.
pub trait SessionSink: Send + Sync {
    /// Database conversation id, if a session is open.
    fn conv_id(&self) -> Option<i64>;

    /// Append one complete model-facing message to the session transcript.
    ///
    /// Built-in durable sinks persist it losslessly (normalized columns plus
    /// the full payload); no message fields are projected away.
    fn append_chat_message(&self, message: &ChatMessage, seq: i64);

    /// Record token usage for a provider/model turn.
    #[allow(clippy::too_many_arguments)]
    fn record_usage(
        &self,
        provider: &str,
        model: &str,
        prompt_tokens: u32,
        completion_tokens: u32,
        cached_tokens: Option<u32>,
        cost: Option<f64>,
        is_estimated: bool,
    );

    /// Persist completed messages at a recoverable mid-turn boundary.
    ///
    /// Returns `true` only when every supplied message is durable. The default
    /// keeps the messages in the turn outcome for its normal end-of-turn commit.
    fn checkpoint_messages(&self, _messages: &[ChatMessage]) -> bool {
        false
    }

    /// Mark the current conversation as ended.
    fn end(&self);

    /// Number of persistence writes that failed since the sink was created.
    ///
    /// Write methods never abort a turn on a flaky disk — they log and move
    /// on. A non-zero count lets a caller (e.g. the TUI) surface to the user
    /// that recent history may be incomplete. Sinks that cannot fail (e.g.
    /// [`NullSessionSink`]) return `0`.
    fn persist_failures(&self) -> u64 {
        0
    }
}

/// Mid-turn checkpoint sink for interactive conversations.
///
/// The daemon already persists the user prompt before starting a turn and owns
/// the final atomic message/usage commit. This sink only writes completed
/// assistant/tool messages at tool boundaries so a process crash cannot discard
/// all completed work from a long-running turn.
pub(crate) struct ToolCheckpointSessionSink {
    db: Mutex<Option<SessionDb>>,
    conv_id: i64,
    failures: AtomicU64,
}

impl ToolCheckpointSessionSink {
    pub(crate) fn open_for(conversation_id: i64) -> Self {
        let db = match SessionDb::open(&db_path()) {
            Ok(db) => Some(db),
            Err(error) => {
                crate::ext::ctx::runtime_warn(format!(
                    "bone: warning: session db open failed (tool checkpoint sink): {error}"
                ));
                None
            }
        };
        Self {
            db: Mutex::new(db),
            conv_id: conversation_id,
            failures: AtomicU64::new(0),
        }
    }
}

impl SessionSink for ToolCheckpointSessionSink {
    fn conv_id(&self) -> Option<i64> {
        Some(self.conv_id)
    }

    fn append_chat_message(&self, _message: &ChatMessage, _seq: i64) {
        // The Driver checkpoints the pending batch explicitly at tool boundaries.
    }

    fn record_usage(
        &self,
        _provider: &str,
        _model: &str,
        _prompt_tokens: u32,
        _completion_tokens: u32,
        _cached_tokens: Option<u32>,
        _cost: Option<f64>,
        _is_estimated: bool,
    ) {
        // RuntimeSession commits usage with the rest of the turn outcome.
    }

    fn checkpoint_messages(&self, messages: &[ChatMessage]) -> bool {
        if messages.is_empty() {
            return true;
        }
        let guard = self.db.lock().unwrap_or_else(|error| error.into_inner());
        let Some(db) = guard.as_ref() else {
            return false;
        };
        match db.append_turn_with_checkpoint(self.conv_id, 0, messages, &[], None) {
            Ok(_) => true,
            Err(error) => {
                self.failures.fetch_add(1, Ordering::Relaxed);
                crate::ext::ctx::runtime_warn(format!(
                    "bone: warning: session db tool checkpoint failed: {error}"
                ));
                false
            }
        }
    }

    fn end(&self) {
        // The authoritative RuntimeSession owns conversation lifecycle.
    }

    fn persist_failures(&self) -> u64 {
        self.failures.load(Ordering::Relaxed)
    }
}

/// A no-op sink that discards everything. `conv_id` is `None`.
///
/// Equivalent to a `SessionWriter` whose DB write failed to open a
/// conversation — every method is a no-op because `conv_id` is `None`.
/// Used by tests and nested agents that must not touch the DB.
pub struct NullSessionSink;

impl SessionSink for NullSessionSink {
    fn conv_id(&self) -> Option<i64> {
        None
    }

    fn append_chat_message(&self, _message: &ChatMessage, _seq: i64) {}

    fn record_usage(
        &self,
        _provider: &str,
        _model: &str,
        _prompt_tokens: u32,
        _completion_tokens: u32,
        _cached_tokens: Option<u32>,
        _cost: Option<f64>,
        _is_estimated: bool,
    ) {
    }

    fn end(&self) {}
}

/// Sink for delegated agents: records token usage against a **parent**
/// conversation, but never appends messages or ends the conversation.
///
/// Nested agents must not create their own top-level chats (that pollutes
/// history with internal prompts). They still burn tokens, and those tokens
/// should appear in `/stats`. This sink is the bridge: `record_usage` writes
/// `usage_events` rows under the parent's `conversation_id`, while
/// `append_chat_message` / `end` stay no-ops.
pub struct UsageOnlySessionSink {
    db: Mutex<Option<SessionDb>>,
    conv_id: i64,
    failures: AtomicU64,
}

impl UsageOnlySessionSink {
    /// Open the default conversations DB and attribute usage to `conversation_id`.
    ///
    /// On open failure the sink still exists but every write no-ops (same
    /// fall-open contract as headless `SessionWriter`).
    pub fn open_for(conversation_id: i64) -> Self {
        let db = match SessionDb::open(&db_path()) {
            Ok(db) => Some(db),
            Err(e) => {
                crate::ext::ctx::runtime_warn(format!(
                    "bone: warning: session db open failed (usage-only sink): {e}"
                ));
                None
            }
        };
        Self::from_parts(db, conversation_id)
    }

    fn from_parts(db: Option<SessionDb>, conversation_id: i64) -> Self {
        Self {
            db: Mutex::new(db),
            conv_id: conversation_id,
            failures: AtomicU64::new(0),
        }
    }

    /// Convenience: `Some(Arc<UsageOnlySessionSink>)` when a parent id is known.
    pub fn for_parent(conversation_id: Option<i64>) -> Option<Arc<dyn SessionSink>> {
        conversation_id.map(|id| Arc::new(Self::open_for(id)) as Arc<dyn SessionSink>)
    }

    fn note_failure(&self, op: &str, err: &rusqlite::Error) {
        self.failures.fetch_add(1, Ordering::Relaxed);
        crate::ext::ctx::runtime_warn(format!("bone: warning: session db {op} failed: {err}"));
    }
}

impl SessionSink for UsageOnlySessionSink {
    fn conv_id(&self) -> Option<i64> {
        Some(self.conv_id)
    }

    fn append_chat_message(&self, _message: &ChatMessage, _seq: i64) {
        // Nested agents must not write transcript rows into the parent chat.
    }

    fn record_usage(
        &self,
        provider: &str,
        model: &str,
        prompt_tokens: u32,
        completion_tokens: u32,
        cached_tokens: Option<u32>,
        cost: Option<f64>,
        is_estimated: bool,
    ) {
        let guard = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let Some(db) = guard.as_ref() else {
            return;
        };
        if let Err(e) = db.record_usage(
            self.conv_id,
            provider,
            model,
            prompt_tokens,
            completion_tokens,
            cached_tokens,
            cost,
            is_estimated,
        ) {
            self.note_failure("record_usage", &e);
        }
    }

    fn end(&self) {
        // Never end the parent conversation when a nested agent finishes.
    }

    fn persist_failures(&self) -> u64 {
        self.failures.load(Ordering::Relaxed)
    }
}

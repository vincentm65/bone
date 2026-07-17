//! Session persistence sink — injectable seam for conversation/usage recording.
//!
//! `SessionSink` is the object-safe trait for the four operations the agent
//! loop performs (`conv_id`, `append_message`, `record_usage`, `end`).
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

use crate::session_db::{SessionDb, db_path};

/// Sink for persisting agent conversation turns and token usage.
///
/// All methods take `&self`; the concrete `SessionWriter` holds a single
/// `Mutex`-guarded connection (write methods lock, mutate, and return `()`),
/// so the trait is object-safe and shareable via `Arc<dyn SessionSink>`.
pub trait SessionSink: Send + Sync {
    /// Database conversation id, if a session is open.
    fn conv_id(&self) -> Option<i64>;

    /// Append a message (user/assistant/tool) to the session transcript.
    /// `is_error` indicates a tool-result that failed; ignored for non-tool roles.
    #[allow(clippy::too_many_arguments)]
    fn append_message(
        &self,
        role: &str,
        content: &str,
        tool_name: Option<&str>,
        tool_call_id: Option<&str>,
        tool_calls: Option<&str>,
        images: Option<&str>,
        is_error: bool,
        seq: i64,
    );

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

    #[allow(clippy::too_many_arguments)]
    fn append_message(
        &self,
        _role: &str,
        _content: &str,
        _tool_name: Option<&str>,
        _tool_call_id: Option<&str>,
        _tool_calls: Option<&str>,
        _images: Option<&str>,
        _is_error: bool,
        _seq: i64,
    ) {
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
/// `append_message` / `end` stay no-ops.
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

    /// Construct with an already-open DB (tests / injected paths).
    pub fn with_db(db: SessionDb, conversation_id: i64) -> Self {
        Self::from_parts(Some(db), conversation_id)
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

    #[allow(clippy::too_many_arguments)]
    fn append_message(
        &self,
        _role: &str,
        _content: &str,
        _tool_name: Option<&str>,
        _tool_call_id: Option<&str>,
        _tool_calls: Option<&str>,
        _images: Option<&str>,
        _is_error: bool,
        _seq: i64,
    ) {
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

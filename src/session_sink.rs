//! Session persistence sink — the injectable seam for conversation/usage
//! recording (Step 3 of the TUI/runtime decoupling).
//!
//! Historically the agent loop owned a private `SessionWriter` struct that
//! opened the SQLite DB on every call. That struct was constructed inside
//! `agent_setup` with no injection point, so the loop could not be driven or
//! unit-tested without a real database file.
//!
//! This module defines `SessionSink` — the object-safe trait capturing the
//! four operations the loop performs (`conv_id`, `append_message`,
//! `record_usage`, `end`). `agent_setup` now accepts an
//! `Option<Arc<dyn SessionSink>>` on `AgentRequest`: when present it is used
//! verbatim; when absent a real `SessionWriter` is constructed as before.
//!
//! `NullSessionSink` is a no-op implementation (matching the `conv_id == None`
//! fast-path `SessionWriter` already had internally), letting tests and a
//! future Driver run the loop with zero side-effects and zero file I/O.

use std::sync::Arc;

/// Sink for persisting agent conversation turns and token usage.
///
/// All methods take `&self` (the concrete `SessionWriter` opens a fresh DB
/// connection per call and never mutates its own state), so the trait is
/// object-safe and shareable via `Arc<dyn SessionSink>`.
pub trait SessionSink: Send + Sync {
    /// Database conversation id, if a session is open.
    fn conv_id(&self) -> Option<i64>;

    /// Append a message (user/assistant/tool) to the session transcript.
    fn append_message(
        &self,
        role: &str,
        content: &str,
        tool_name: Option<&str>,
        tool_call_id: Option<&str>,
        tool_calls: Option<&str>,
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
}

/// A no-op sink that discards everything. `conv_id` is `None`.
///
/// Equivalent to a `SessionWriter` whose DB write failed to open a
/// conversation — every method is a no-op because `conv_id` is `None`.
/// Used by tests and (eventually) a Driver that does not persist locally.
pub struct NullSessionSink;

impl SessionSink for NullSessionSink {
    fn conv_id(&self) -> Option<i64> {
        None
    }

    fn append_message(
        &self,
        _role: &str,
        _content: &str,
        _tool_name: Option<&str>,
        _tool_call_id: Option<&str>,
        _tool_calls: Option<&str>,
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

/// Convenience: wrap any `SessionSink` in an `Arc` for injection.
pub fn shared<S: SessionSink + 'static>(sink: S) -> Arc<dyn SessionSink> {
    Arc::new(sink)
}

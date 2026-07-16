//! Step 3: prove the `SessionSink` trait is the injectable seam for session
//! persistence.
//!
//! The agent loop owned a private `SessionWriter` that opened SQLite on every
//! call. With no injection point the loop couldn't be driven without a real
//! database file. `SessionSink` is the object-safe trait that replaces it,
//! and `NullSessionSink` is the no-op implementation (matching the
//! `conv_id == None` fast-path `SessionWriter` already had).
//!
//! These tests verify:
//! 1. The trait is externally implementable (a recording sink).
//! 2. `NullSessionSink` is provably inert.
//! 3. Sinks compose through `Arc<dyn SessionSink>` (object-safe, shareable).
//! 4. `UsageOnlySessionSink` attributes usage to a parent conversation without
//!    writing messages or ending that conversation.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use bone_core::session_db::SessionDb;
use bone_core::session_sink::{NullSessionSink, SessionSink, UsageOnlySessionSink};

/// A recording sink that captures every call for later inspection.
struct RecordingSink {
    conv: Option<i64>,
    messages: Mutex<Vec<String>>,
    usages: Mutex<u32>,
    ended: Mutex<bool>,
}

impl RecordingSink {
    fn new() -> Self {
        Self {
            conv: Some(42),
            messages: Mutex::new(Vec::new()),
            usages: Mutex::new(0),
            ended: Mutex::new(false),
        }
    }
}

impl SessionSink for RecordingSink {
    fn conv_id(&self) -> Option<i64> {
        self.conv
    }

    fn append_message(
        &self,
        role: &str,
        content: &str,
        _tool_name: Option<&str>,
        _tool_call_id: Option<&str>,
        _tool_calls: Option<&str>,
        _images: Option<&str>,
        _seq: i64,
    ) {
        self.messages
            .lock()
            .unwrap()
            .push(format!("{role}: {content}"));
    }

    fn record_usage(
        &self,
        _provider: &str,
        _model: &str,
        prompt_tokens: u32,
        _completion_tokens: u32,
        _cached_tokens: Option<u32>,
        _cost: Option<f64>,
        _is_estimated: bool,
    ) {
        *self.usages.lock().unwrap() += prompt_tokens;
    }

    fn end(&self) {
        *self.ended.lock().unwrap() = true;
    }
}

#[test]
fn trait_is_externally_implementable_and_records() {
    let sink = RecordingSink::new();
    sink.append_message("user", "hello", None, None, None, None, 0);
    sink.append_message("assistant", "hi there", None, None, None, None, 1);
    sink.record_usage("openai", "gpt-4", 100, 50, None, None, false);
    sink.end();

    assert_eq!(
        sink.messages.lock().unwrap().as_slice(),
        &["user: hello".to_string(), "assistant: hi there".to_string()]
    );
    assert_eq!(*sink.usages.lock().unwrap(), 100);
    assert!(*sink.ended.lock().unwrap());
}

#[test]
fn null_sink_is_inert() {
    let sink = NullSessionSink;
    // conv_id is None — matching SessionWriter when DB is unavailable.
    assert_eq!(sink.conv_id(), None);

    // Every write method must be a no-op (not panic).
    sink.append_message("user", "ignored", None, None, None, None, 0);
    sink.record_usage("p", "m", 1, 1, None, None, false);
    sink.end();
    // Nothing to assert beyond "didn't panic" — that IS the contract.
}

#[test]
fn sink_is_object_safe_via_arc_dyn() {
    // Arc<dyn SessionSink> is the injection type on AgentRequest.
    let sink: Arc<dyn SessionSink> = Arc::new(RecordingSink::new());
    assert_eq!(sink.conv_id(), Some(42));
    sink.append_message("user", "test", None, None, None, None, 0);
    assert_eq!(sink.conv_id(), Some(42)); // still works after a call
}

#[test]
fn null_sink_is_object_safe_via_arc_dyn() {
    let sink: Arc<dyn SessionSink> = Arc::new(NullSessionSink);
    assert_eq!(sink.conv_id(), None);
    sink.end();
}

#[test]
fn mixed_sink_types_unify_under_dyn() {
    // A Driver could hold a Vec of sinks of different concrete types.
    let sinks: Vec<Arc<dyn SessionSink>> =
        vec![Arc::new(NullSessionSink), Arc::new(RecordingSink::new())];
    assert_eq!(sinks[0].conv_id(), None);
    assert_eq!(sinks[1].conv_id(), Some(42));
}

#[test]
fn injected_sink_is_shareable_via_arc() {
    // Arc refcount — mirrors the Step 0 provider injection test.
    let sink: Arc<dyn SessionSink> = Arc::new(NullSessionSink);
    let cloned = sink.clone();
    assert_eq!(Arc::strong_count(&sink), 2);
    assert_eq!(cloned.conv_id(), None);
    drop(cloned);
    assert_eq!(Arc::strong_count(&sink), 1);
}

static USAGE_SINK_TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_session_db() -> (SessionDb, std::path::PathBuf) {
    let id = USAGE_SINK_TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "bone_usage_only_sink_{}_{}",
        std::process::id(),
        id
    ));
    let db = SessionDb::open(&path).unwrap();
    (db, path)
}

#[test]
fn usage_only_sink_records_usage_against_parent_without_messages() {
    let (db, path) = temp_session_db();
    let parent_id = db
        .create_conversation("parent-provider", "parent-model")
        .unwrap();

    // Re-open via a second connection so the sink owns its own handle (mirrors
    // production: nested agent opens the shared conversations.db path).
    drop(db);
    let sink_db = SessionDb::open(&path).unwrap();
    let sink = UsageOnlySessionSink::with_db(sink_db, parent_id);

    assert_eq!(sink.conv_id(), Some(parent_id));

    // Messages must not land in the parent transcript.
    sink.append_message(
        "user",
        "internal subagent prompt — must not persist",
        None,
        None,
        None,
        None,
        1,
    );
    sink.append_message(
        "assistant",
        "internal subagent reply — must not persist",
        None,
        None,
        None,
        None,
        2,
    );
    // Must not end (or delete) the parent conversation.
    sink.end();

    sink.record_usage(
        "sub-provider",
        "sub-model",
        120,
        40,
        Some(10),
        Some(0.02),
        false,
    );
    sink.record_usage("sub-provider", "sub-model", 80, 20, None, None, true);
    assert_eq!(sink.persist_failures(), 0);

    let verify = SessionDb::open(&path).unwrap();
    assert_eq!(
        verify.max_message_seq(parent_id).unwrap(),
        0,
        "usage-only sink must not append parent messages"
    );

    let usage = verify.conversation_usage(parent_id).unwrap();
    assert_eq!(usage.prompt_tokens, 200);
    assert_eq!(usage.completion_tokens, 60);
    assert_eq!(usage.cached_tokens, 10);
    assert!((usage.cost - 0.02).abs() < f64::EPSILON);
    assert_eq!(usage.request_count, 2);

    let by_model = verify.usage_by_provider(parent_id).unwrap();
    assert_eq!(by_model.len(), 1);
    assert_eq!(by_model[0].provider, "sub-provider");
    assert_eq!(by_model[0].model, "sub-model");

    // Parent conversation must still exist after end().
    assert!(verify.conversation_exists(parent_id).unwrap());

    let _ = std::fs::remove_file(&path);
}

#[test]
fn usage_only_for_parent_is_none_without_id() {
    assert!(UsageOnlySessionSink::for_parent(None).is_none());
    let sink = UsageOnlySessionSink::for_parent(Some(99)).expect("Some id must yield a sink");
    assert_eq!(sink.conv_id(), Some(99));
}

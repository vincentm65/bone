//! Phase 2 acceptance: the core `Driver` runs a full turn headless, with no
//! terminal, no real provider, and no DB — proving the agent loop now lives in
//! one reusable place. Drives the `Driver` directly with a scripted
//! `MockProvider`, `ExtensionManager::unloaded()`, builtin tools, and a
//! `NullSessionSink`, then asserts the emitted `AgentRunEvent` sequence.

use async_trait::async_trait;
use futures_util::StreamExt; // for .boxed()
use std::sync::{Arc, Mutex};

mod common;

use bone_core::agent::AgentRunEvent;
use bone_core::chat::build_chat_history;
use bone_core::ext::{BootOptions, ExtensionManager, boot_with_tools};
use bone_core::llm::provider::{LlmProvider, ProviderRequestContext};
use bone_core::llm::{
    ChatEvent, ChatMessage, ChatRole, LlmError, LlmErrorKind, ResponseStream, TokenStats,
};
use bone_core::pane_content::KeyRequest;
use bone_core::runtime::{
    ApprovalReplyRegistry, ChannelApprovalGate, Driver, KeyReplyRegistry, LocalConn,
    RuntimeCommand, RuntimeConn, RuntimeEvent,
};
use bone_core::session_db::SessionDb;
use bone_core::session_sink::{NullSessionSink, SessionSink, UsageOnlySessionSink};
use bone_core::tools::registry::ToolHandler;
use bone_core::tools::types::{Tool, ToolExecutionContext, ToolOutput};
use bone_core::tools::{
    ApprovalGate, ApprovalMode, AutoApprovalGate, CallOutcome, ToolCall, ToolDefinition,
    builtin_tools,
};

/// Deterministic provider that replays one scripted stream per `chat_stream`
/// call. After the script is drained, subsequent calls yield an empty stream
/// (no text, no tool calls) — which the loop treats as a final empty turn.
/// A single `chat_stream` call: either a stream of events or a
/// connection-level error (so tests can exercise the connection retry path).
enum MockAttempt {
    Stream(Vec<Result<ChatEvent, LlmError>>),
    Pending,
    ConnErr(LlmError),
}

struct MockProvider {
    model: String,
    script: Mutex<Vec<MockAttempt>>,
}

impl MockProvider {
    fn new(model: &str, script: Vec<ChatEvent>) -> Self {
        Self::new_raw(
            model,
            vec![MockAttempt::Stream(script.into_iter().map(Ok).collect())],
        )
    }

    /// Per-call scripts; later calls pop in reverse order. A `ConnErr`
    /// attempt makes `chat_stream` itself return `Err`.
    fn new_raw(model: &str, attempts: Vec<MockAttempt>) -> Self {
        Self {
            model: model.to_string(),
            script: Mutex::new(attempts.into_iter().rev().collect()),
        }
    }
}

#[async_trait]
impl LlmProvider for MockProvider {
    fn id(&self) -> &str {
        "mock"
    }
    fn name(&self) -> &str {
        "Mock Provider"
    }
    fn model(&self) -> &str {
        &self.model
    }
    fn set_model(&mut self, model: String) {
        self.model = model;
    }
    async fn chat_stream(
        &self,
        _messages: Vec<ChatMessage>,
        _tools: Vec<ToolDefinition>,
    ) -> Result<ResponseStream, LlmError> {
        let attempt = self
            .script
            .lock()
            .unwrap()
            .pop()
            .unwrap_or(MockAttempt::Stream(vec![]));
        match attempt {
            MockAttempt::Stream(events) => Ok(futures_util::stream::iter(events).boxed()),
            MockAttempt::Pending => Ok(futures_util::stream::pending().boxed()),
            MockAttempt::ConnErr(e) => Err(e),
        }
    }
}

fn driver_with(script: Vec<ChatEvent>, mode: ApprovalMode) -> (Driver, &'static str) {
    driver_with_gate(script, mode, Arc::new(AutoApprovalGate))
}

/// Phase: the frontend transport. Drive a full turn through a `LocalConn` —
/// the same surface the TUI renders from — instead of calling `Driver::run`
/// directly. Proves `next_event()` streams the Driver's events, signals turn end
/// with `None`, and hands back the reclaimable `DriverOutcome` via `take_outcome`.
#[tokio::test]
async fn local_conn_streams_turn_and_yields_outcome() {
    use bone_core::runtime::{
        ApprovalReplyRegistry, KeyReplyRegistry, LocalConn, RuntimeCommand, RuntimeConn,
    };
    use std::sync::atomic::AtomicBool;

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
    let (mut driver, prompt) = driver_with(
        vec![
            ChatEvent::TextDelta("hello ".into()),
            ChatEvent::TextDelta("world".into()),
        ],
        ApprovalMode::Safe,
    );
    driver.runtime_events = Some(tx.clone());

    let mut conn = LocalConn::new(
        rx,
        tx,
        driver,
        Arc::new(AtomicBool::new(false)),
        ApprovalReplyRegistry::new(),
        KeyReplyRegistry::new(),
        Arc::new(std::sync::Mutex::new(None)),
    );
    conn.send(RuntimeCommand::SubmitPrompt {
        request_id: None,
        text: prompt.to_string(),
        images: vec![],
    });

    // Pump exactly like the TUI loop: collect events until `None` (idle).
    let mut events = Vec::new();
    while let Some(ev) = conn.next_event().await {
        events.push(ev);
    }

    assert!(
        matches!(events.first(), Some(RuntimeEvent::Started { .. })),
        "first event is Started, got {events:?}"
    );
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            RuntimeEvent::TextDelta { text } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "hello world", "text deltas stream through the conn");
    assert!(
        matches!(events.last(), Some(RuntimeEvent::Finished { content }) if content == "hello world"),
        "last event is Finished, got {events:?}"
    );

    // The reclaimable outcome is available once the turn drained.
    let outcome = conn.take_outcome().expect("outcome after turn end");
    assert_eq!(
        outcome.result.expect("ok result").content,
        "hello world",
        "conn hands back the Driver's authoritative final content"
    );
    assert!(conn.is_finished());
}

/// Phase: the persistent session. Two turns run through one `RuntimeSession`
/// via `build_driver` → `LocalConn` → `apply_outcome`. Proves the session is the
/// cross-turn owner: the transcript accumulates both turns and token stats carry
/// forward — the daemon (and the TUI) rely on this instead of a per-turn struct.
#[tokio::test]
async fn runtime_session_accumulates_state_across_turns() {
    use bone_core::runtime::{
        ApprovalReplyRegistry, KeyReplyRegistry, LocalConn, RuntimeCommand, RuntimeConn,
        RuntimeSession,
    };
    use bone_core::tools::SharedApprovalMode;
    use std::sync::atomic::AtomicBool;

    // One shared provider scripts a distinct reply per turn (the script pops
    // across `chat_stream` calls, so it spans both turns).
    let llm: Arc<dyn LlmProvider> = Arc::new(MockProvider::new_raw(
        "mock-1",
        vec![
            MockAttempt::Stream(vec![Ok(ChatEvent::TextDelta("first".into()))]),
            MockAttempt::Stream(vec![Ok(ChatEvent::TextDelta("second".into()))]),
        ],
    ));
    let mut session = RuntimeSession::new(ToolHandler::new(builtin_tools()));

    async fn run_turn(session: &mut RuntimeSession, llm: Arc<dyn LlmProvider>, prompt: &str) {
        // The Driver appends the user message itself; mark where this turn's new
        // messages begin, then drive the turn through a LocalConn.
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
        let driver = session.build_driver(
            llm,
            ExtensionManager::unloaded(),
            common::config_store(),
            SharedApprovalMode::new(ApprovalMode::Safe),
            Arc::new(AutoApprovalGate),
            tx.clone(),
            KeyReplyRegistry::new(),
            Arc::new(AtomicBool::new(false)),
            Arc::new(NullSessionSink) as Arc<dyn SessionSink>,
        );
        let mut conn = LocalConn::new(
            rx,
            tx,
            driver,
            Arc::new(AtomicBool::new(false)),
            ApprovalReplyRegistry::new(),
            KeyReplyRegistry::new(),
            Arc::new(std::sync::Mutex::new(None)),
        );
        conn.send(RuntimeCommand::SubmitPrompt {
            request_id: None,
            text: prompt.to_string(),
            images: vec![],
        });
        while conn.next_event().await.is_some() {}
        let outcome = conn.take_outcome().expect("turn produced an outcome");
        let (result, persistence_error) = session.apply_outcome(outcome);
        result.expect("turn ok");
        assert!(persistence_error.is_none());
    }

    run_turn(&mut session, llm.clone(), "hi").await;
    assert_eq!(
        session.transcript.len(),
        2,
        "turn 1: user + assistant in transcript"
    );
    let after_turn1 = session.token_stats.received;

    run_turn(&mut session, llm.clone(), "bye").await;
    // Both turns are retained in order: user/assistant ×2.
    let roles: Vec<_> = session.transcript.iter().map(|m| m.role).collect();
    assert_eq!(
        roles,
        vec![
            ChatRole::User,
            ChatRole::Assistant,
            ChatRole::User,
            ChatRole::Assistant
        ],
        "session accumulates both turns across the conversation"
    );
    assert!(
        session.token_stats.received >= after_turn1,
        "token stats carry forward across turns ({} then {})",
        after_turn1,
        session.token_stats.received
    );
}

fn driver_with_gate(
    script: Vec<ChatEvent>,
    mode: ApprovalMode,
    gate: Arc<dyn ApprovalGate>,
) -> (Driver, &'static str) {
    let prompt = "hi";
    let transcript = vec![ChatMessage::new(ChatRole::User, prompt)];
    let history = build_chat_history(&transcript, None);
    let driver = Driver {
        llm: Arc::new(MockProvider::new("mock-1", script)),
        extensions: ExtensionManager::unloaded(),
        tools: ToolHandler::new(builtin_tools()),
        session: Arc::new(NullSessionSink) as Arc<dyn SessionSink>,
        gate,
        approval_mode: bone_core::tools::SharedApprovalMode::new(mode),
        agent_depth: 0,
        activity: None,
        on_token_usage: None,
        events: false,
        event_sender: None,
        runtime_events: None,
        key_reply_registry: None,
        cancel: None,
        history,
        transcript,
        token_stats: TokenStats::new(),
        system_prompt_override: None,
        conversation_id: None,
        config_store: common::config_store(),
        turn_nudge: Arc::new(std::sync::Mutex::new(None)),
    };
    (driver, prompt)
}

/// Build a driver from per-call scripts (`MockAttempt`s), popped in reverse
/// order so the first element is the first `chat_stream` call.
fn driver_with_raw(attempts: Vec<MockAttempt>, mode: ApprovalMode) -> (Driver, &'static str) {
    let prompt = "hi";
    let transcript = vec![ChatMessage::new(ChatRole::User, prompt)];
    let history = build_chat_history(&transcript, None);
    let driver = Driver {
        llm: Arc::new(MockProvider::new_raw("mock-1", attempts)),
        extensions: ExtensionManager::unloaded(),
        tools: ToolHandler::new(builtin_tools()),
        session: Arc::new(NullSessionSink) as Arc<dyn SessionSink>,
        gate: Arc::new(AutoApprovalGate),
        approval_mode: bone_core::tools::SharedApprovalMode::new(mode),
        agent_depth: 0,
        activity: None,
        on_token_usage: None,
        events: false,
        event_sender: None,
        runtime_events: None,
        key_reply_registry: None,
        cancel: None,
        history,
        transcript,
        token_stats: TokenStats::new(),
        system_prompt_override: None,
        conversation_id: None,
        config_store: common::config_store(),
        turn_nudge: Arc::new(std::sync::Mutex::new(None)),
    };
    (driver, prompt)
}

fn collect_events(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AgentRunEvent>,
) -> Vec<AgentRunEvent> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        out.push(ev);
    }
    out
}

fn collect_runtime_events(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<RuntimeEvent>,
) -> Vec<RuntimeEvent> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        out.push(ev);
    }
    out
}

#[tokio::test]
async fn driver_runs_simple_turn_to_completion() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (mut driver, prompt) = driver_with(
        vec![
            ChatEvent::TextDelta("hello ".into()),
            ChatEvent::TextDelta("world".into()),
            ChatEvent::TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 2,
                cached_tokens: None,
                cost: None,
            },
        ],
        ApprovalMode::Safe,
    );
    driver.event_sender = Some(tx);

    let response = driver.run(prompt).await.expect("driver run");
    assert_eq!(response.content, "hello world");

    let events = collect_events(&mut rx);
    // First event is Started, last is Finished with the assembled content.
    assert!(
        matches!(events.first(), Some(AgentRunEvent::Started { .. })),
        "first event must be Started, got {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentRunEvent::TokenUsage { sent, .. } if *sent == 10)),
        "must emit TokenUsage with the scripted prompt tokens"
    );
    assert!(
        matches!(events.last(), Some(AgentRunEvent::Finished { content }) if content == "hello world"),
        "last event must be Finished with the full content, got {events:?}"
    );
}

#[tokio::test]
async fn driver_outcome_carries_usage_records() {
    // The TUI runs the Driver with a NullSessionSink and persists usage events
    // from the returned outcome, so the outcome must surface per-request usage.
    let (driver, prompt) = driver_with(
        vec![
            ChatEvent::TextDelta("hi".into()),
            ChatEvent::TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 2,
                cached_tokens: Some(4),
                cost: None,
            },
        ],
        ApprovalMode::Safe,
    );

    let outcome = driver.run_to_outcome(prompt).await;
    assert_eq!(outcome.usage.len(), 1, "one provider-reported usage record");
    let u = &outcome.usage[0];
    assert_eq!(u.prompt_tokens, 10);
    assert_eq!(u.completion_tokens, 2);
    assert_eq!(u.cached_tokens, Some(4));
    assert!(!u.is_estimated, "provider-reported usage is not estimated");
}

/// Nested agents inject `UsageOnlySessionSink` so their driver-reported usage
/// lands in the parent conversation's `usage_events` (what `/stats` reads).
#[tokio::test]
async fn driver_usage_only_sink_persists_to_parent_conversation() {
    let path = std::env::temp_dir().join(format!(
        "bone_driver_usage_only_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let parent_id = {
        let db = SessionDb::open(&path).unwrap();
        db.create_conversation("parent", "parent-model").unwrap()
    };
    let sink_db = SessionDb::open(&path).unwrap();
    let sink: Arc<dyn SessionSink> = Arc::new(UsageOnlySessionSink::with_db(sink_db, parent_id));

    let prompt = "hi";
    let transcript = vec![ChatMessage::new(ChatRole::User, prompt)];
    let history = build_chat_history(&transcript, None);
    let driver = Driver {
        llm: Arc::new(MockProvider::new(
            "sub-model",
            vec![
                ChatEvent::TextDelta("nested answer".into()),
                ChatEvent::TokenUsage {
                    prompt_tokens: 50,
                    completion_tokens: 12,
                    cached_tokens: Some(5),
                    cost: Some(0.001),
                },
            ],
        )),
        extensions: ExtensionManager::unloaded(),
        tools: ToolHandler::new(builtin_tools()),
        session: sink,
        gate: Arc::new(AutoApprovalGate),
        approval_mode: bone_core::tools::SharedApprovalMode::new(ApprovalMode::Safe),
        agent_depth: 1,
        activity: None,
        on_token_usage: None,
        events: false,
        event_sender: None,
        runtime_events: None,
        key_reply_registry: None,
        cancel: None,
        history,
        transcript,
        token_stats: TokenStats::new(),
        system_prompt_override: None,
        conversation_id: Some(parent_id),
        config_store: common::config_store(),
        turn_nudge: Arc::new(std::sync::Mutex::new(None)),
    };

    let outcome = driver.run_to_outcome(prompt).await;
    assert!(outcome.result.is_ok(), "nested turn should succeed");
    assert_eq!(outcome.usage.len(), 1);

    let verify = SessionDb::open(&path).unwrap();
    assert_eq!(
        verify.max_message_seq(parent_id).unwrap(),
        0,
        "nested agent must not append parent transcript rows"
    );
    let usage = verify.conversation_usage(parent_id).unwrap();
    assert_eq!(usage.prompt_tokens, 50);
    assert_eq!(usage.completion_tokens, 12);
    assert_eq!(usage.cached_tokens, 5);
    assert_eq!(usage.request_count, 1);
    assert!((usage.cost - 0.001).abs() < 1e-9);

    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn driver_outcome_usage_falls_back_to_estimate() {
    // When the provider streams no TokenUsage, the Driver estimates and still
    // records a (flagged) usage entry in the outcome.
    let (driver, prompt) = driver_with(vec![ChatEvent::TextDelta("hi".into())], ApprovalMode::Safe);

    let outcome = driver.run_to_outcome(prompt).await;
    assert_eq!(outcome.usage.len(), 1, "one estimated usage record");
    assert!(
        outcome.usage[0].is_estimated,
        "missing provider usage falls back to an estimate"
    );
}

#[tokio::test]
async fn driver_executes_tool_call_then_finishes() {
    // Turn 1: the model requests a read-only tool (allowed in Safe mode). The
    // file does not exist, so the tool returns an error result — but the point
    // is the ToolCall→ToolResult flow runs through the gate and tools. Turn 2:
    // the script is exhausted, so the loop sees no tool calls and finishes.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (mut driver, prompt) = driver_with(
        vec![ChatEvent::ToolCall(ToolCall {
            id: "call_1".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({ "path": "/nonexistent/bone-driver-test" }),
        })],
        ApprovalMode::Safe,
    );
    driver.event_sender = Some(tx);

    let response = driver.run(prompt).await.expect("driver run with tool");
    // Second (empty) turn produces no assistant text.
    assert_eq!(response.content, "");

    let events = collect_events(&mut rx);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentRunEvent::ToolCall { name, .. } if name == "read_file")),
        "must emit a ToolCall event for read_file, got {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentRunEvent::ToolResult { name, .. } if name == "read_file")),
        "must emit a ToolResult event for read_file, got {events:?}"
    );
}

/// Phase: protocol approval. The Driver consults a `ChannelApprovalGate` for
/// every tool call; the gate emits a `RuntimeEvent::ApprovalRequest` and the
/// "frontend" (the test) answers by resolving the `ApprovalReplyRegistry` by id.
/// Approve → the tool runs (success on a real temp file). Deny → the tool is
/// skipped with an error result, even though Safe mode would have auto-allowed
/// this read-only call (the frontend reply overrides policy).
async fn run_with_channel_decision(label: &str, decision: CallOutcome, expect_error: bool) {
    // A real, readable file so an Approved read_file succeeds (is_error=false).
    let path = std::env::temp_dir().join(format!("bone-approval-test-{label}"));
    std::fs::write(&path, "hello").unwrap();

    let registry = bone_core::runtime::ApprovalReplyRegistry::new();
    let (evtx, mut evrx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
    let (etx, mut erx) = tokio::sync::mpsc::unbounded_channel::<AgentRunEvent>();
    let gate: Arc<dyn ApprovalGate> =
        Arc::new(ChannelApprovalGate::new(evtx, registry.clone(), None, None));
    let (mut driver, prompt) = driver_with_gate(
        vec![ChatEvent::ToolCall(ToolCall {
            id: "call_1".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({ "path": path.to_string_lossy() }),
        })],
        ApprovalMode::Safe,
        gate,
    );
    driver.event_sender = Some(etx);

    let run = tokio::spawn(async move { driver.run(prompt).await });

    // Act as the frontend: receive the approval request event and reply by id.
    let ev = tokio::time::timeout(std::time::Duration::from_secs(5), evrx.recv())
        .await
        .expect("approval request timed out")
        .expect("approval request");
    let RuntimeEvent::ApprovalRequest {
        id,
        name,
        auto_allows,
        ..
    } = ev
    else {
        panic!("expected ApprovalRequest, got {ev:?}");
    };
    assert_eq!(name, "read_file");
    assert!(auto_allows, "read-only in Safe mode is auto-allowed");
    assert!(registry.resolve(id, decision), "reply routed to the gate");

    let response = run.await.unwrap().expect("driver run");
    assert_eq!(response.content, "", "second (empty) turn finishes");

    // The reply decided whether the tool ran (success on a real file) or was
    // skipped (error result) — proving channel approval overrides auto-allow.
    let mut tool_error: Option<bool> = None;
    while let Ok(ev) = erx.try_recv() {
        if let AgentRunEvent::ToolResult { name, is_error, .. } = ev
            && name == "read_file"
        {
            tool_error = Some(is_error);
        }
    }
    assert_eq!(
        tool_error,
        Some(expect_error),
        "tool result error-ness must match the channel decision"
    );

    std::fs::remove_file(&path).ok();
}

#[tokio::test]
async fn local_cancel_unblocks_pending_approval() {
    use std::sync::atomic::AtomicBool;

    let approvals = ApprovalReplyRegistry::new();
    let keys = KeyReplyRegistry::new();
    let (events_tx, events_rx) = tokio::sync::mpsc::unbounded_channel();
    let gate: Arc<dyn ApprovalGate> = Arc::new(ChannelApprovalGate::new(
        events_tx.clone(),
        approvals.clone(),
        None,
        None,
    ));
    let (mut driver, prompt) = driver_with_gate(
        vec![ChatEvent::ToolCall(ToolCall {
            id: "call_1".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({"path": "missing"}),
        })],
        ApprovalMode::Safe,
        gate,
    );
    let cancel = Arc::new(AtomicBool::new(false));
    driver.cancel = Some(cancel.clone());
    driver.runtime_events = Some(events_tx.clone());
    driver.key_reply_registry = Some(keys.clone());
    let mut conn = LocalConn::new(
        events_rx,
        events_tx,
        driver,
        cancel,
        approvals.clone(),
        keys,
        Arc::new(Mutex::new(None)),
    );
    conn.send(RuntimeCommand::SubmitPrompt {
        request_id: None,
        text: prompt.into(),
        images: vec![],
    });

    loop {
        let event = tokio::time::timeout(std::time::Duration::from_secs(5), conn.next_event())
            .await
            .expect("approval request timed out")
            .expect("turn ended before approval");
        if matches!(event, RuntimeEvent::ApprovalRequest { .. }) {
            break;
        }
    }
    assert_eq!(approvals.pending_count(), 1);
    conn.send(RuntimeCommand::Cancel);

    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while conn.next_event().await.is_some() {}
    })
    .await
    .expect("cancel left approval blocked");
    assert_eq!(approvals.pending_count(), 0);
}

struct KeyTool;

#[async_trait]
impl Tool for KeyTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "key_tool".into(),
            description: "waits for a key".into(),
            input_schema: serde_json::json!({ "type": "object" }),
        }
    }
    async fn execute(&self, _arguments: serde_json::Value) -> Result<String, String> {
        Ok("done".into())
    }
    async fn execute_output_live(
        &self,
        _arguments: serde_json::Value,
        events: Option<tokio::sync::mpsc::UnboundedSender<KeyRequest>>,
        _context: ToolExecutionContext,
    ) -> Result<ToolOutput, String> {
        let Some(tx) = events else {
            return Ok(ToolOutput::text("no events".into()));
        };
        let (reply, rx) = tokio::sync::oneshot::channel();
        tx.send(KeyRequest { reply }).unwrap();
        let key = rx.await.unwrap();
        Ok(ToolOutput::text(key.code))
    }
}

#[tokio::test]
async fn driver_key_reply_completes_turn() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
    let registry = bone_core::runtime::KeyReplyRegistry::new();
    let prompt = "hi";
    let transcript = vec![ChatMessage::new(ChatRole::User, prompt)];
    let history = build_chat_history(&transcript, None);
    let driver = Driver {
        llm: Arc::new(MockProvider::new(
            "mock-1",
            vec![ChatEvent::ToolCall(ToolCall {
                id: "c1".into(),
                name: "key_tool".into(),
                arguments: serde_json::json!({}),
            })],
        )),
        extensions: ExtensionManager::unloaded(),
        tools: ToolHandler::new(builtin_tools().register(KeyTool)),
        session: Arc::new(NullSessionSink) as Arc<dyn SessionSink>,
        gate: Arc::new(AutoApprovalGate),
        approval_mode: bone_core::tools::SharedApprovalMode::new(ApprovalMode::Danger),
        agent_depth: 0,
        activity: None,
        on_token_usage: None,
        events: false,
        event_sender: None,
        runtime_events: Some(tx),
        key_reply_registry: Some(registry.clone()),
        cancel: None,
        history,
        transcript,
        token_stats: TokenStats::new(),
        system_prompt_override: None,
        conversation_id: None,
        config_store: common::config_store(),
        turn_nudge: Arc::new(std::sync::Mutex::new(None)),
    };

    let run = tokio::spawn(async move { driver.run(prompt).await });
    let id = loop {
        if let RuntimeEvent::KeyRequest { id } =
            tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
                .await
                .expect("runtime event timed out")
                .expect("runtime event")
        {
            break id;
        }
    };
    assert!(registry.resolve(
        id,
        bone_core::pane_content::KeyEvent {
            code: "Enter".into(),
            char: None,
            ctrl: false,
            alt: false,
            shift: false,
        }
    ));

    tokio::time::timeout(std::time::Duration::from_secs(5), run)
        .await
        .expect("driver wedged after key reply")
        .unwrap()
        .expect("driver run");
}

#[tokio::test]
async fn driver_emits_rich_runtime_event_stream() {
    // The interactive frontend (TUI / RPC client) consumes `runtime_events`:
    // Started → TextDelta… → TokenUsage → Finished. This is what Step 3's TUI
    // cutover renders instead of reimplementing the loop.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
    let (mut driver, prompt) = driver_with(
        vec![
            ChatEvent::TextDelta("hello ".into()),
            ChatEvent::TextDelta("world".into()),
            ChatEvent::TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 2,
                cached_tokens: None,
                cost: None,
            },
        ],
        ApprovalMode::Safe,
    );
    driver.runtime_events = Some(tx);

    let response = driver.run(prompt).await.expect("driver run");
    assert_eq!(response.content, "hello world");

    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }
    assert!(
        matches!(events.first(), Some(RuntimeEvent::Started { .. })),
        "first runtime event is Started, got {events:?}"
    );
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            RuntimeEvent::TextDelta { text } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "hello world", "text deltas reassemble the message");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, RuntimeEvent::TokenUsage { sent, .. } if *sent == 10)),
        "emits TokenUsage"
    );
    assert!(
        matches!(events.last(), Some(RuntimeEvent::Finished { content }) if content == "hello world"),
        "last runtime event is Finished, got {events:?}"
    );
}

#[tokio::test]
async fn channel_gate_approve_runs_tool() {
    run_with_channel_decision("approve", CallOutcome::Approve, false).await;
}

#[tokio::test]
async fn channel_gate_deny_skips_tool() {
    run_with_channel_decision("deny", CallOutcome::Denied, true).await;
}

// A `before_turn` hook can now surface live status to the attached frontend:
// the Driver threads its `runtime_events` sender into the hook ctx as
// `runtime_status`, and `ctx.ui.status` emits a `RuntimeEvent::Status`. This is
// the channel auto-compaction uses to announce "Compacting…/Compacted: …".
#[tokio::test]
async fn driver_before_turn_status_surfaces_to_runtime_events() {
    let config_dir = common::temp_dir("driver-before-turn-status");
    std::fs::create_dir_all(&config_dir).unwrap();
    // Register a before_turn hook that announces via ctx.ui.status.
    std::fs::write(
        config_dir.join("init.lua"),
        r#"
bone.on("before_turn", function(_event, ctx)
    if ctx and ctx.ui and ctx.ui.status then
        ctx.ui.status("from before_turn hook")
    end
end)
"#,
    )
    .unwrap();

    let config = common::config_store();
    let booted = boot_with_tools(
        &config_dir,
        &config_dir,
        &config,
        false,
        BootOptions::default(),
        "test-model",
        "TestProvider",
    );

    let prompt = "hi";
    let transcript = vec![ChatMessage::new(ChatRole::User, prompt)];
    let history = build_chat_history(&transcript, None);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();

    let driver = Driver {
        llm: Arc::new(MockProvider::new(
            "mock-1",
            vec![ChatEvent::TextDelta("ok".into())],
        )),
        extensions: booted.manager,
        tools: booted.tools,
        session: Arc::new(NullSessionSink) as Arc<dyn SessionSink>,
        gate: Arc::new(AutoApprovalGate),
        approval_mode: bone_core::tools::SharedApprovalMode::new(ApprovalMode::Safe),
        agent_depth: 0,
        activity: None,
        on_token_usage: None,
        events: false,
        event_sender: None,
        runtime_events: Some(tx),
        key_reply_registry: None,
        cancel: None,
        history,
        transcript,
        token_stats: TokenStats::new(),
        system_prompt_override: None,
        conversation_id: None,
        config_store: common::config_store(),
        turn_nudge: Arc::new(std::sync::Mutex::new(None)),
    };

    let response = driver.run(prompt).await.expect("driver run");
    assert_eq!(response.content, "ok");

    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }
    assert!(
        events.iter().any(|e| matches!(
            e,
            RuntimeEvent::Status { message } if message == "from before_turn hook"
        )),
        "before_turn ctx.ui.status should surface as a RuntimeEvent::Status; got {events:?}",
    );

    std::fs::remove_dir_all(&config_dir).ok();
}

/// Records the exact message list of every `chat_stream` call while replaying
/// a scripted stream per call — for asserting what the provider actually sees.
struct CapturingProvider {
    model: String,
    script: Mutex<Vec<Vec<ChatEvent>>>,
    captured: Mutex<Vec<Vec<ChatMessage>>>,
}

#[async_trait]
impl LlmProvider for CapturingProvider {
    fn id(&self) -> &str {
        "mock"
    }
    fn name(&self) -> &str {
        "Capturing Provider"
    }
    fn model(&self) -> &str {
        &self.model
    }
    fn set_model(&mut self, model: String) {
        self.model = model;
    }
    async fn chat_stream(
        &self,
        messages: Vec<ChatMessage>,
        _tools: Vec<ToolDefinition>,
    ) -> Result<ResponseStream, LlmError> {
        self.captured.lock().unwrap().push(messages);
        let events = self.script.lock().unwrap().pop().unwrap_or_default();
        Ok(futures_util::stream::iter(events.into_iter().map(Ok)).boxed())
    }
}

#[tokio::test]
async fn driver_appends_lua_prompt_text_to_configured_main_prompt() {
    let config_dir = common::temp_dir("driver-configured-system-prompt");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("init.lua"),
        r#"
bone.on("before_turn", function()
    return { system_prompt_append = "Lua turn instructions" }
end)
"#,
    )
    .unwrap();

    let config = common::config_store();
    let booted = boot_with_tools(
        &config_dir,
        &config_dir,
        &config,
        false,
        BootOptions::default(),
        "test-model",
        "TestProvider",
    );
    let prompt = "hi";
    let transcript = vec![ChatMessage::new(ChatRole::User, prompt)];
    let base = bone_core::llm::prompts::system_prompt_with_base(Some("Configured base"));
    let llm = Arc::new(CapturingProvider {
        model: "mock-1".into(),
        script: Mutex::new(vec![vec![ChatEvent::TextDelta("done".into())]]),
        captured: Mutex::new(Vec::new()),
    });
    let driver = Driver {
        llm: llm.clone(),
        extensions: booted.manager,
        tools: ToolHandler::new(builtin_tools()),
        session: Arc::new(NullSessionSink),
        gate: Arc::new(AutoApprovalGate),
        approval_mode: bone_core::tools::SharedApprovalMode::new(ApprovalMode::Safe),
        agent_depth: 0,
        activity: None,
        on_token_usage: None,
        events: false,
        event_sender: None,
        runtime_events: None,
        key_reply_registry: None,
        cancel: None,
        history: build_chat_history(&transcript, Some(&base)),
        transcript,
        token_stats: TokenStats::new(),
        system_prompt_override: Some(base.clone()),
        conversation_id: None,
        config_store: config,
        turn_nudge: Arc::new(Mutex::new(None)),
    };

    let outcome = driver.run_to_outcome(prompt).await;
    assert_eq!(outcome.result.unwrap().content, "done");
    let captured = llm.captured.lock().unwrap();
    assert_eq!(captured.len(), 1);
    assert_eq!(
        captured[0][0].content,
        format!("{base}\n\nLua turn instructions")
    );

    std::fs::remove_dir_all(config_dir).ok();
}

struct EphemeralImageTool {
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl Tool for EphemeralImageTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "ephemeral_image".into(),
            description: "returns a transient image".into(),
            input_schema: serde_json::json!({ "type": "object" }),
        }
    }

    async fn execute(&self, _arguments: serde_json::Value) -> Result<String, String> {
        unreachable!("execute_output is used")
    }

    async fn execute_output(&self, _arguments: serde_json::Value) -> Result<ToolOutput, String> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        Ok(ToolOutput {
            content: format!("screenshot {call}"),
            images: vec![bone_core::llm::ImageData {
                media_type: "image/jpeg".into(),
                data: format!("ephemeral-base64-{call}"),
                width: Some(100 + call as u32),
                height: Some(200 + call as u32),
                sha256: Some(format!("sha256-{call}")),
            }],
            ephemeral_images: true,
            ..Default::default()
        })
    }
}

#[tokio::test]
async fn driver_keeps_only_latest_ephemeral_image_in_request_history() {
    let prompt = "observe";
    let transcript = vec![ChatMessage::new(ChatRole::User, prompt)];
    let history = build_chat_history(&transcript, None);
    let llm = Arc::new(CapturingProvider {
        model: "mock-vision".into(),
        script: Mutex::new(vec![
            vec![ChatEvent::TextDelta("done".into())],
            vec![ChatEvent::ToolCall(ToolCall {
                id: "image-2".into(),
                name: "ephemeral_image".into(),
                arguments: serde_json::json!({}),
            })],
            vec![ChatEvent::ToolCall(ToolCall {
                id: "image-1".into(),
                name: "ephemeral_image".into(),
                arguments: serde_json::json!({}),
            })],
        ]),
        captured: Mutex::new(Vec::new()),
    });
    let driver = Driver {
        llm: llm.clone(),
        extensions: ExtensionManager::unloaded(),
        tools: ToolHandler::new(builtin_tools().register(EphemeralImageTool {
            calls: std::sync::atomic::AtomicUsize::new(0),
        })),
        session: Arc::new(NullSessionSink) as Arc<dyn SessionSink>,
        gate: Arc::new(AutoApprovalGate),
        approval_mode: bone_core::tools::SharedApprovalMode::new(ApprovalMode::Danger),
        agent_depth: 0,
        activity: None,
        on_token_usage: None,
        events: false,
        event_sender: None,
        runtime_events: None,
        key_reply_registry: None,
        cancel: None,
        history,
        transcript,
        token_stats: TokenStats::new(),
        system_prompt_override: None,
        conversation_id: None,
        config_store: common::config_store(),
        turn_nudge: Arc::new(Mutex::new(None)),
    };

    let outcome = driver.run_to_outcome(prompt).await;
    assert_eq!(outcome.result.as_ref().unwrap().content, "done");

    let captured = llm.captured.lock().unwrap();
    assert_eq!(captured.len(), 3);
    let images = |messages: &[ChatMessage]| {
        messages
            .iter()
            .flat_map(|message| message.images.iter().cloned())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        images(&captured[1]),
        vec![bone_core::llm::ImageData {
            media_type: "image/jpeg".into(),
            data: "ephemeral-base64-1".into(),
            width: Some(101),
            height: Some(201),
            sha256: Some("sha256-1".into()),
        }]
    );
    assert_eq!(
        images(&captured[2]),
        vec![bone_core::llm::ImageData {
            media_type: "image/jpeg".into(),
            data: "ephemeral-base64-2".into(),
            width: Some(102),
            height: Some(202),
            sha256: Some("sha256-2".into()),
        }]
    );

    for message in outcome
        .transcript
        .iter()
        .chain(outcome.persist_messages.iter())
    {
        assert!(message.images.is_empty());
        assert!(!message.content.contains("ephemeral-base64"));
    }
}

#[tokio::test]
async fn driver_propagates_parent_context_to_lua_tools() {
    let config_dir = common::temp_dir("driver-lua-tool-context");
    let tools_dir = config_dir.join("lua/tools");
    std::fs::create_dir_all(&tools_dir).unwrap();
    std::fs::write(
        tools_dir.join("inspect_context.lua"),
        r#"
bone.tool.register({
  name = "inspect_context",
  description = "returns the live parent context",
  safety = "read_only",
  parameters = { type = "object", properties = {} },
  execute = function(_args, ctx)
    local info = ctx.runtime.info()
    return tostring(info.session_id) .. ":" .. tostring(info.provider) .. ":" .. tostring(info.model)
  end,
})
"#,
    )
    .unwrap();

    let config = common::config_store();
    let booted = boot_with_tools(
        &config_dir,
        &config_dir,
        &config,
        false,
        BootOptions::default(),
        "boot-model",
        "BootProvider",
    );
    let prompt = "inspect";
    let transcript = vec![ChatMessage::new(ChatRole::User, prompt)];
    let history = build_chat_history(&transcript, None);
    let llm = Arc::new(CapturingProvider {
        model: "mock-ctx".into(),
        script: Mutex::new(vec![
            vec![ChatEvent::TextDelta("done".into())],
            vec![ChatEvent::ToolCall(ToolCall {
                id: "ctx-1".into(),
                name: "inspect_context".into(),
                arguments: serde_json::json!({}),
            })],
        ]),
        captured: Mutex::new(Vec::new()),
    });
    let driver = Driver {
        llm: llm.clone(),
        extensions: booted.manager,
        tools: booted.tools,
        session: Arc::new(NullSessionSink) as Arc<dyn SessionSink>,
        gate: Arc::new(AutoApprovalGate),
        approval_mode: bone_core::tools::SharedApprovalMode::new(ApprovalMode::Safe),
        agent_depth: 0,
        activity: None,
        on_token_usage: None,
        events: false,
        event_sender: None,
        runtime_events: None,
        key_reply_registry: None,
        cancel: None,
        history,
        transcript,
        token_stats: TokenStats::new(),
        system_prompt_override: None,
        conversation_id: Some(77),
        config_store: common::config_store(),
        turn_nudge: Arc::new(std::sync::Mutex::new(None)),
    };

    driver.run(prompt).await.expect("driver run");

    let captured = llm.captured.lock().unwrap();
    let tool_result = captured[1]
        .iter()
        .find(|message| message.role == ChatRole::Tool)
        .expect("second request contains Lua tool result");
    assert_eq!(tool_result.content, "77:mock:mock-ctx");

    std::fs::remove_dir_all(&config_dir).ok();
}

#[tokio::test]
async fn driver_uses_dynamic_safety_for_extension_hooks_and_approval() {
    let config_dir = common::temp_dir("driver-dynamic-safety");
    let tools_dir = config_dir.join("lua/tools");
    std::fs::create_dir_all(&tools_dir).unwrap();
    std::fs::write(
        config_dir.join("init.lua"),
        r#"
bone.on("tool_call", function(event)
  _TOOL_SAFETY = event.safety
  if event.safety ~= "read_only" then
    return { block = true, reason = "unexpected safety: " .. tostring(event.safety) }
  end
end)
"#,
    )
    .unwrap();
    std::fs::write(
        tools_dir.join("dynamic_read.lua"),
        r#"
bone.tool.register({
  name = "dynamic_read",
  description = "read-only dynamically registered tool",
  safety = "read_only",
  parameters = { type = "object", properties = {} },
  execute = function() return "dynamic-ok" end,
})
"#,
    )
    .unwrap();

    let config = common::config_store();
    let booted = boot_with_tools(
        &config_dir,
        &config_dir,
        &config,
        false,
        BootOptions::default(),
        "boot-model",
        "BootProvider",
    );
    let lua = booted.manager.lua_arc();
    let prompt = "inspect";
    let transcript = vec![ChatMessage::new(ChatRole::User, prompt)];
    let history = build_chat_history(&transcript, None);
    let llm = Arc::new(CapturingProvider {
        model: "mock-dynamic".into(),
        script: Mutex::new(vec![
            vec![ChatEvent::TextDelta("done".into())],
            vec![ChatEvent::ToolCall(ToolCall {
                id: "dynamic-1".into(),
                name: "dynamic_read".into(),
                arguments: serde_json::json!({}),
            })],
        ]),
        captured: Mutex::new(Vec::new()),
    });
    let driver = Driver {
        llm: llm.clone(),
        extensions: booted.manager,
        tools: booted.tools,
        session: Arc::new(NullSessionSink) as Arc<dyn SessionSink>,
        gate: Arc::new(AutoApprovalGate),
        approval_mode: bone_core::tools::SharedApprovalMode::new(ApprovalMode::Safe),
        agent_depth: 0,
        activity: None,
        on_token_usage: None,
        events: false,
        event_sender: None,
        runtime_events: None,
        key_reply_registry: None,
        cancel: None,
        history,
        transcript,
        token_stats: TokenStats::new(),
        system_prompt_override: None,
        conversation_id: None,
        config_store: common::config_store(),
        turn_nudge: Arc::new(std::sync::Mutex::new(None)),
    };

    driver.run(prompt).await.expect("driver run");

    let captured = llm.captured.lock().unwrap();
    let tool_result = captured[1]
        .iter()
        .find(|message| message.role == ChatRole::Tool)
        .expect("second request contains dynamic tool result");
    assert_eq!(tool_result.content, "dynamic-ok");
    drop(captured);
    let safety: String = lua
        .lock()
        .unwrap()
        .globals()
        .get("_TOOL_SAFETY")
        .expect("tool_call hook recorded safety");
    assert_eq!(safety, "read_only");

    std::fs::remove_dir_all(&config_dir).ok();
}

#[tokio::test]
async fn driver_keeps_tool_preamble_as_assistant_content() {
    let prompt = "hi";
    let transcript = vec![ChatMessage::new(ChatRole::User, prompt)];
    let history = build_chat_history(&transcript, None);
    let llm = Arc::new(CapturingProvider {
        model: "mock-1".into(),
        script: Mutex::new(vec![
            vec![ChatEvent::TextDelta("done".into())],
            vec![
                ChatEvent::TextDelta("I'll run read_file now.".into()),
                ChatEvent::ToolCall(ToolCall {
                    id: "c1".into(),
                    name: "read_file".into(),
                    arguments: serde_json::json!({ "path": "/nonexistent/bone-driver-test" }),
                }),
            ],
        ]),
        captured: Mutex::new(Vec::new()),
    });

    let driver = Driver {
        llm: llm.clone(),
        extensions: ExtensionManager::unloaded(),
        tools: ToolHandler::new(builtin_tools()),
        session: Arc::new(NullSessionSink) as Arc<dyn SessionSink>,
        gate: Arc::new(AutoApprovalGate),
        approval_mode: bone_core::tools::SharedApprovalMode::new(ApprovalMode::Safe),
        agent_depth: 0,
        activity: None,
        on_token_usage: None,
        events: false,
        event_sender: None,
        runtime_events: None,
        key_reply_registry: None,
        cancel: None,
        history,
        transcript,
        token_stats: TokenStats::new(),
        system_prompt_override: None,
        conversation_id: None,
        config_store: common::config_store(),
        turn_nudge: Arc::new(std::sync::Mutex::new(None)),
    };

    let response = driver.run(prompt).await.expect("driver run");
    assert_eq!(response.content, "done");

    let captured = llm.captured.lock().unwrap();
    assert_eq!(
        captured.len(),
        2,
        "tool call should trigger a second request"
    );
    let assistant = captured[1]
        .iter()
        .find(|m| m.role == ChatRole::Assistant && !m.tool_calls.is_empty())
        .expect("second request includes assistant tool-call message");
    assert_eq!(assistant.content, "I'll run read_file now.");
    assert_eq!(assistant.tool_calls[0].name, "read_file");
}

// A `before_turn` hook can return `turn_message`: transient guidance retained in
// request-only history for the rest of the user turn. The first round appends a
// trailing user item; a later update rides in the final tool result so no fresh
// user turn causes provider chat templates to drop echoed in-turn reasoning.
// Retaining each marker at its original position makes later requests extend the
// previous provider-cache prefix without persisting markers to the transcript.
#[tokio::test]
async fn driver_turn_message_is_trailing_and_not_persisted() {
    let config_dir = common::temp_dir("driver-turn-message");
    std::fs::create_dir_all(&config_dir).unwrap();
    // The marker changes every round (like a live task list) so persistence of
    // an old round's message into the next request is detectable.
    std::fs::write(
        config_dir.join("init.lua"),
        r#"
_N = 0
bone.on("before_turn", function(_event, _ctx)
    _N = _N + 1
    return { turn_message = "TM-MARKER-" .. _N }
end)
"#,
    )
    .unwrap();

    let config = common::config_store();
    let booted = boot_with_tools(
        &config_dir,
        &config_dir,
        &config,
        false,
        BootOptions::default(),
        "test-model",
        "TestProvider",
    );

    let prompt = "hi";
    let transcript = vec![ChatMessage::new(ChatRole::User, prompt)];
    let history = build_chat_history(&transcript, None);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();

    // Round 1 requests a tool call, round 2 finishes — two provider requests.
    let llm = Arc::new(CapturingProvider {
        model: "mock-1".into(),
        script: Mutex::new(vec![
            vec![ChatEvent::TextDelta("done".into())],
            vec![ChatEvent::ToolCall(ToolCall {
                id: "c1".into(),
                name: "big_tool".into(),
                arguments: serde_json::json!({}),
            })],
        ]),
        captured: Mutex::new(Vec::new()),
    });

    let driver = Driver {
        llm: llm.clone(),
        extensions: booted.manager,
        tools: ToolHandler::new(builtin_tools().register(BigTool)),
        session: Arc::new(NullSessionSink) as Arc<dyn SessionSink>,
        gate: Arc::new(AutoApprovalGate),
        approval_mode: bone_core::tools::SharedApprovalMode::new(ApprovalMode::Danger),
        agent_depth: 0,
        activity: None,
        on_token_usage: None,
        events: false,
        event_sender: None,
        runtime_events: Some(tx),
        key_reply_registry: None,
        cancel: None,
        history,
        transcript,
        token_stats: TokenStats::new(),
        system_prompt_override: None,
        conversation_id: None,
        config_store: common::config_store(),
        turn_nudge: Arc::new(std::sync::Mutex::new(None)),
    };

    let response = driver.run(prompt).await.expect("driver run");

    let captured = llm.captured.lock().unwrap();
    assert_eq!(captured.len(), 2, "two provider requests expected");

    let first_last = captured[0].last().expect("first request has messages");
    assert_eq!(first_last.role, ChatRole::User);
    assert!(first_last.content.contains("TM-MARKER-1"));

    let second = &captured[1];
    let first_marker = second
        .iter()
        .position(|m| m.content.contains("TM-MARKER-1"))
        .expect("first marker remains at its original position");
    let assistant = second
        .iter()
        .position(|m| m.role == ChatRole::Assistant)
        .expect("second request contains the tool-calling assistant");
    assert!(
        first_marker < assistant,
        "the retained marker must precede newly appended assistant/tool messages"
    );
    let second_last = second.last().expect("second request has messages");
    assert_eq!(second_last.role, ChatRole::Tool);
    assert!(second_last.content.contains("TM-MARKER-2"));

    let occurrences: usize = second
        .iter()
        .filter(|m| m.content.contains("TM-MARKER-"))
        .count();
    assert_eq!(occurrences, 2, "request-only markers should accumulate");
    assert!(
        response
            .transcript
            .iter()
            .all(|m| !m.content.contains("TM-MARKER-")),
        "request-only markers must not enter the persisted transcript"
    );

    std::fs::remove_dir_all(&config_dir).ok();
}

#[tokio::test]
async fn driver_before_turn_can_read_canonical_command_enablement() {
    let config_dir = common::temp_dir("driver-before-turn-command-config");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("init.lua"),
        r#"
bone.on("before_turn", function(_event, ctx)
    local commands = ctx.config.get_table("commands")
    if type(commands) ~= "table" then return nil end
    if type(commands.disabled) == "table" then
        for _, name in ipairs(commands.disabled) do
            if name == "compact" then return nil end
        end
    end
    return {
        action = "conversation.replace",
        messages = { { role = "user", content = "auto-compacted" } },
    }
end)
"#,
    )
    .unwrap();

    let config = common::config_store();
    let booted = boot_with_tools(
        &config_dir,
        &config_dir,
        &config,
        false,
        BootOptions::default(),
        "test-model",
        "TestProvider",
    );
    let prompt = "hi";
    let transcript = vec![ChatMessage::new(ChatRole::User, prompt)];
    let driver = Driver {
        llm: Arc::new(MockProvider::new(
            "mock-1",
            vec![ChatEvent::TextDelta("done".into())],
        )),
        extensions: booted.manager,
        tools: ToolHandler::new(builtin_tools()),
        session: Arc::new(NullSessionSink),
        gate: Arc::new(AutoApprovalGate),
        approval_mode: bone_core::tools::SharedApprovalMode::new(ApprovalMode::Safe),
        agent_depth: 0,
        activity: None,
        on_token_usage: None,
        events: false,
        event_sender: None,
        runtime_events: None,
        key_reply_registry: None,
        cancel: None,
        history: build_chat_history(&transcript, None),
        transcript,
        token_stats: TokenStats::new(),
        system_prompt_override: None,
        conversation_id: None,
        config_store: common::config_store(),
        turn_nudge: Arc::new(Mutex::new(None)),
    };

    let outcome = driver.run_to_outcome(prompt).await;
    assert!(outcome.transcript_replaced);
    assert_eq!(outcome.transcript[0].content, "auto-compacted");
    assert_eq!(outcome.transcript.last().unwrap().content, "done");

    std::fs::remove_dir_all(&config_dir).ok();
}

/// Tool that returns a very large result, to prove compaction sees the *current*
/// pending context mid-loop (including appended tool results), not a stale
/// last-request size.
struct BigTool;

#[async_trait]
impl Tool for BigTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "big_tool".into(),
            description: "returns a large result".into(),
            input_schema: serde_json::json!({ "type": "object" }),
        }
    }
    async fn execute(&self, _arguments: serde_json::Value) -> Result<String, String> {
        Ok("x".repeat(200_000))
    }
}

// Regression: before_turn's `ctx.usage.snapshot().context_length` must reflect
// the *current* pending history (with tool results appended mid-loop), not the
// stale last-request size. Without the per-iteration refresh in the Driver, the
// threshold check lags by one round, compaction never fires mid tool-call
// sequence, and the next request overshoots the model's context limit.
#[tokio::test]
async fn driver_before_turn_sees_current_context_mid_loop() {
    let config_dir = common::temp_dir("driver-before-turn-context");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("init.lua"),
        r#"
_OBS = {}
bone.on("before_turn", function(_event, ctx)
    local cl = 0
    if ctx and ctx.usage and ctx.usage.snapshot then
        local snap = ctx.usage.snapshot()
        if snap then cl = snap.context_length or 0 end
    end
    _OBS[#_OBS + 1] = cl
end)
"#,
    )
    .unwrap();

    let config = common::config_store();
    let booted = boot_with_tools(
        &config_dir,
        &config_dir,
        &config,
        false,
        BootOptions::default(),
        "test-model",
        "TestProvider",
    );
    let lua_arc = booted.manager.lua_arc();

    let prompt = "hi";
    let transcript = vec![ChatMessage::new(ChatRole::User, prompt)];
    let history = build_chat_history(&transcript, None);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();

    let driver = Driver {
        llm: Arc::new(MockProvider::new(
            "mock-1",
            vec![ChatEvent::ToolCall(ToolCall {
                id: "c1".into(),
                name: "big_tool".into(),
                arguments: serde_json::json!({}),
            })],
        )),
        extensions: booted.manager,
        tools: ToolHandler::new(builtin_tools().register(BigTool)),
        session: Arc::new(NullSessionSink) as Arc<dyn SessionSink>,
        gate: Arc::new(AutoApprovalGate),
        approval_mode: bone_core::tools::SharedApprovalMode::new(ApprovalMode::Danger),
        agent_depth: 0,
        activity: None,
        on_token_usage: None,
        events: false,
        event_sender: None,
        runtime_events: Some(tx),
        key_reply_registry: None,
        cancel: None,
        history,
        transcript,
        token_stats: TokenStats::new(),
        system_prompt_override: None,
        conversation_id: None,
        config_store: common::config_store(),
        turn_nudge: Arc::new(std::sync::Mutex::new(None)),
    };

    driver.run(prompt).await.expect("driver run");

    // Read the recorded context_length observations from Lua.
    let lua = lua_arc.lock().unwrap();
    let obs: mlua::Table = lua.globals().get("_OBS").expect("_OBS set");
    let observations: Vec<i64> = obs.sequence_values().filter_map(|v| v.ok()).collect();
    drop(lua);

    assert!(
        observations.len() >= 2,
        "before_turn should fire at least twice (init + after tool result); got {observations:?}",
    );
    let first = observations[0];
    let second = observations[1];
    // big_tool appended ~200_000 chars (~52k tokens). The 2nd before_turn
    // observation must reflect that growth — proving the snapshot is the current
    // pending context, not a stale last-request size (which would show ~no
    // growth between the two observations).
    assert!(
        second > first + 10_000,
        "2nd before_turn context_length ({second}) must exceed the 1st ({first}) \
         by the appended tool result (~52k tokens); a stale snapshot would show ~no growth",
    );

    std::fs::remove_dir_all(&config_dir).ok();
}
// --- Connection-level retry path (chat_stream returns Err) ---

#[tokio::test]
async fn driver_retries_connection_error_then_succeeds() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (mut driver, prompt) = driver_with_raw(
        vec![
            MockAttempt::ConnErr(LlmError::new_with_kind(
                LlmErrorKind::Connection,
                "dns failure",
            )),
            MockAttempt::Stream(vec![Ok(ChatEvent::TextDelta("recovered".into()))]),
        ],
        ApprovalMode::Safe,
    );
    driver.runtime_events = Some(tx);

    let response = driver.run(prompt).await.expect("driver run");
    assert_eq!(response.content, "recovered");

    let events = collect_runtime_events(&mut rx);
    assert!(
        events.iter().any(|e| matches!(e,
            RuntimeEvent::Status { message } if message.starts_with("retry"))),
        "should emit retry status for connection error; got {events:?}"
    );
}

#[tokio::test]
async fn driver_connection_error_exhausts_retries() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let conn_err = || LlmError::new_with_kind(LlmErrorKind::Connection, "server down");
    let (mut driver, prompt) = driver_with_raw(
        vec![
            MockAttempt::ConnErr(conn_err()),
            MockAttempt::ConnErr(conn_err()),
            MockAttempt::ConnErr(conn_err()),
        ],
        ApprovalMode::Safe,
    );
    driver.runtime_events = Some(tx);

    let result = driver.run(prompt).await;
    let err = result.err().expect("should fail after exhausting retries");
    assert!(
        err.contains("provider error after 3 attempts"),
        "should report exhausted retries; got {err}"
    );

    let events = collect_runtime_events(&mut rx);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, RuntimeEvent::Failed { .. })),
        "should emit Failed runtime event; got {events:?}"
    );
}

// --- Mid-stream error abort limit ---

#[tokio::test]
async fn driver_aborts_after_five_consecutive_stream_errors() {
    let err = || LlmError::new_with_kind(LlmErrorKind::Connection, "reset");
    // Five attempts, each yielding a single mid-stream error.
    let (driver, prompt) = driver_with_raw(
        (0..5)
            .map(|_| MockAttempt::Stream(vec![Err::<ChatEvent, _>(err())]))
            .collect(),
        ApprovalMode::Safe,
    );

    let result = driver.run(prompt).await;
    let err = result.err().expect("should abort after 5 errors");
    assert!(
        err.contains("5 consecutive stream errors"),
        "should report the 5-error abort; got {err}"
    );
}

// --- Mid-stream error then successful retry ---

#[tokio::test]
async fn driver_retries_after_stream_error_and_discards_partial() {
    // Attempt 1: partial text then mid-stream error.
    // Attempt 2: clean text — the transcript should contain ONLY this.
    let (driver, prompt) = driver_with_raw(
        vec![
            MockAttempt::Stream(vec![
                Ok(ChatEvent::TextDelta("partial".into())),
                Err(LlmError::new_with_kind(LlmErrorKind::Connection, "reset")),
            ]),
            MockAttempt::Stream(vec![Ok(ChatEvent::TextDelta("final".into()))]),
        ],
        ApprovalMode::Safe,
    );

    let outcome = driver.run_to_outcome(prompt).await;
    assert_eq!(outcome.result.unwrap().content, "final");
    assert_eq!(
        outcome.transcript.last().unwrap().content,
        "final",
        "transcript should contain only the successful attempt's text"
    );
}

#[tokio::test]
async fn driver_does_not_retry_non_retryable_stream_error() {
    let (driver, prompt) = driver_with_raw(
        vec![
            MockAttempt::Stream(vec![Err(LlmError::new_with_kind(
                LlmErrorKind::Config,
                "codex response incomplete: max_output_tokens",
            ))]),
            MockAttempt::Stream(vec![Ok(ChatEvent::TextDelta("should not retry".into()))]),
        ],
        ApprovalMode::Safe,
    );

    let result = driver.run(prompt).await;
    let err = result.err().expect("should fail without retrying");
    assert!(err.contains("max_output_tokens"), "unexpected error: {err}");
}

// --- Conversation compaction ---

#[derive(Clone)]
struct CompactionCapture {
    messages: Vec<ChatMessage>,
    tools: Vec<ToolDefinition>,
    context: ProviderRequestContext,
}

struct CompactionProvider {
    attempts: Mutex<Vec<MockAttempt>>,
    captures: Mutex<Vec<CompactionCapture>>,
}

impl CompactionProvider {
    fn new(attempts: Vec<MockAttempt>) -> Self {
        Self {
            attempts: Mutex::new(attempts.into_iter().rev().collect()),
            captures: Mutex::new(Vec::new()),
        }
    }

    fn respond(&self) -> Result<ResponseStream, LlmError> {
        match self
            .attempts
            .lock()
            .unwrap()
            .pop()
            .unwrap_or(MockAttempt::Stream(Vec::new()))
        {
            MockAttempt::Stream(events) => Ok(futures_util::stream::iter(events).boxed()),
            MockAttempt::Pending => Ok(futures_util::stream::pending().boxed()),
            MockAttempt::ConnErr(error) => Err(error),
        }
    }
}

#[async_trait]
impl LlmProvider for CompactionProvider {
    fn id(&self) -> &str {
        "compaction-mock"
    }

    fn name(&self) -> &str {
        "Compaction Mock"
    }

    fn model(&self) -> &str {
        "compaction-model"
    }

    fn set_model(&mut self, _model: String) {}

    async fn chat_stream(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<ResponseStream, LlmError> {
        self.captures.lock().unwrap().push(CompactionCapture {
            messages,
            tools,
            context: ProviderRequestContext::default(),
        });
        self.respond()
    }

    async fn chat_stream_with_context(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
        context: ProviderRequestContext,
    ) -> Result<ResponseStream, LlmError> {
        self.captures.lock().unwrap().push(CompactionCapture {
            messages,
            tools,
            context,
        });
        self.respond()
    }
}

fn compaction_test_driver(
    name: &str,
    lua_source: &str,
    llm: Arc<CompactionProvider>,
    transcript: Vec<ChatMessage>,
    runtime_events: Option<tokio::sync::mpsc::UnboundedSender<RuntimeEvent>>,
) -> (Driver, std::path::PathBuf) {
    let config_dir = common::temp_dir(name);
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("init.lua"), lua_source).unwrap();
    let config = common::config_store();
    let booted = boot_with_tools(
        &config_dir,
        &config_dir,
        &config,
        false,
        BootOptions::default(),
        "compaction-model",
        "Compaction Mock",
    );
    let base = "Configured compaction base".to_string();
    let history = build_chat_history(&transcript, Some(&base));
    (
        Driver {
            llm,
            extensions: booted.manager,
            config_store: config,
            tools: ToolHandler::new(builtin_tools()),
            session: Arc::new(NullSessionSink),
            gate: Arc::new(AutoApprovalGate),
            approval_mode: bone_core::tools::SharedApprovalMode::new(ApprovalMode::Safe),
            agent_depth: 0,
            activity: None,
            on_token_usage: None,
            events: false,
            event_sender: None,
            runtime_events,
            key_reply_registry: None,
            cancel: None,
            history,
            transcript,
            token_stats: TokenStats::new(),
            system_prompt_override: Some(base),
            conversation_id: Some(42),
            turn_nudge: Arc::new(Mutex::new(None)),
        },
        config_dir,
    )
}

#[tokio::test]
async fn compaction_success_uses_final_context_and_keeps_private_output_private() {
    let lua = r#"
bone.on("before_turn", function()
    return {
        system_prompt_append = "append-one",
        turn_message = "TRANSIENT-MARKER",
        tool_filter = {},
        conversation = { compact = { instruction = "FIRST-INSTRUCTION", keep_recent_turns = 1 } },
    }
end)
bone.on("before_turn", function()
    return {
        system_prompt_append = "append-two",
        conversation = { compact = { instruction = "SECOND-INSTRUCTION", keep_recent_turns = 0 } },
    }
end)
"#;
    let llm = Arc::new(CompactionProvider::new(vec![
        MockAttempt::Stream(vec![Ok(ChatEvent::TextDelta("PRIVATE-CHECKPOINT".into()))]),
        MockAttempt::Stream(vec![Ok(ChatEvent::TextDelta("normal answer".into()))]),
    ]));
    let prompt = "current";
    let transcript = vec![
        ChatMessage::new(ChatRole::User, "u1"),
        ChatMessage::new(ChatRole::Assistant, "a1"),
        ChatMessage::new(ChatRole::User, "u2"),
        ChatMessage::new(ChatRole::Assistant, "a2"),
        ChatMessage::new(ChatRole::User, prompt),
    ];
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (driver, config_dir) = compaction_test_driver(
        "driver-compaction-success",
        lua,
        llm.clone(),
        transcript,
        Some(tx),
    );

    let outcome = driver.run_to_outcome(prompt).await;
    assert_eq!(outcome.result.as_ref().unwrap().content, "normal answer");
    assert!(outcome.transcript_replaced);
    assert_eq!(outcome.transcript.len(), 5);
    assert_eq!(outcome.transcript[0].role, ChatRole::User);
    assert!(outcome.transcript[0].content.contains("PRIVATE-CHECKPOINT"));
    assert_eq!(outcome.transcript[1].content, "u2");
    assert_eq!(outcome.transcript[2].content, "a2");
    assert_eq!(outcome.transcript[3].content, prompt);

    let captures = llm.captures.lock().unwrap();
    assert_eq!(captures.len(), 2);
    let private = &captures[0];
    let normal = &captures[1];
    assert!(
        private.messages[0]
            .content
            .contains("Configured compaction base")
    );
    assert!(private.messages[0].content.contains("append-one"));
    assert!(private.messages[0].content.contains("append-two"));
    assert!(
        private
            .messages
            .last()
            .unwrap()
            .content
            .contains("FIRST-INSTRUCTION")
    );
    assert!(
        !private
            .messages
            .last()
            .unwrap()
            .content
            .contains("SECOND-INSTRUCTION")
    );
    assert!(
        private
            .messages
            .iter()
            .all(|message| !message.content.contains("TRANSIENT-MARKER"))
    );
    assert!(
        normal
            .messages
            .iter()
            .any(|message| message.content.contains("TRANSIENT-MARKER"))
    );
    assert!(!private.tools.is_empty());
    assert!(normal.tools.is_empty());
    assert_eq!(private.context.conversation_id, Some(42));
    assert_eq!(normal.context.conversation_id, Some(42));
    assert!(Arc::ptr_eq(
        private.context.turn_state.as_ref().unwrap(),
        normal.context.turn_state.as_ref().unwrap()
    ));
    drop(captures);

    let mut surfaced = String::new();
    while let Ok(event) = rx.try_recv() {
        if let RuntimeEvent::TextDelta { text } = event {
            surfaced.push_str(&text);
        }
    }
    assert_eq!(surfaced, "normal answer");
    assert_eq!(outcome.usage.len(), 2);
    assert!(outcome.usage.iter().all(|usage| usage.is_estimated));
    std::fs::remove_dir_all(config_dir).ok();
}

#[tokio::test]
async fn compaction_usage_is_counted_like_normal_requests() {
    let lua = r#"
bone.on("before_turn", function()
    return { conversation = { compact = { instruction = "summarize", keep_recent_turns = 0 } } }
end)
"#;
    let llm = Arc::new(CompactionProvider::new(vec![
        MockAttempt::Stream(vec![
            Ok(ChatEvent::TextDelta("checkpoint".into())),
            Ok(ChatEvent::TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 2,
                cached_tokens: Some(1),
                cost: Some(0.1),
            }),
        ]),
        MockAttempt::Stream(vec![
            Ok(ChatEvent::TextDelta("done".into())),
            Ok(ChatEvent::TokenUsage {
                prompt_tokens: 30,
                completion_tokens: 4,
                cached_tokens: Some(2),
                cost: Some(0.2),
            }),
        ]),
    ]));
    let prompt = "current";
    let transcript = vec![
        ChatMessage::new(ChatRole::User, "old"),
        ChatMessage::new(ChatRole::Assistant, "old answer"),
        ChatMessage::new(ChatRole::User, prompt),
    ];
    let (driver, config_dir) =
        compaction_test_driver("driver-compaction-usage", lua, llm, transcript, None);

    let outcome = driver.run_to_outcome(prompt).await;
    assert_eq!(outcome.token_stats.request_count, 2);
    assert_eq!(outcome.token_stats.sent, 40);
    assert_eq!(outcome.token_stats.received, 6);
    assert_eq!(outcome.token_stats.cached, 3);
    assert!((outcome.token_stats.cost - 0.3).abs() < f64::EPSILON);
    assert_eq!(outcome.usage.len(), 2);
    assert!(outcome.usage.iter().all(|usage| !usage.is_estimated));
    assert_eq!(outcome.usage[0].prompt_tokens, 10);
    assert_eq!(outcome.usage[1].prompt_tokens, 30);
    std::fs::remove_dir_all(config_dir).ok();
}

#[tokio::test]
async fn compaction_tool_output_repairs_once_without_executing_private_call() {
    let lua = r#"
bone.on("before_turn", function()
    return { conversation = { compact = { instruction = "summarize", keep_recent_turns = 0 } } }
end)
"#;
    let llm = Arc::new(CompactionProvider::new(vec![
        MockAttempt::Stream(vec![Ok(ChatEvent::ToolCall(ToolCall {
            id: "private-call".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({ "path": "must-not-run" }),
        }))]),
        MockAttempt::Stream(vec![Ok(ChatEvent::TextDelta("repaired checkpoint".into()))]),
        MockAttempt::Stream(vec![Ok(ChatEvent::TextDelta("done".into()))]),
    ]));
    let prompt = "current";
    let transcript = vec![
        ChatMessage::new(ChatRole::User, "old"),
        ChatMessage::new(ChatRole::Assistant, "old answer"),
        ChatMessage::new(ChatRole::User, prompt),
    ];
    let (driver, config_dir) = compaction_test_driver(
        "driver-compaction-repair",
        lua,
        llm.clone(),
        transcript,
        None,
    );

    let outcome = driver.run_to_outcome(prompt).await;
    assert_eq!(outcome.result.as_ref().unwrap().content, "done");
    assert!(outcome.transcript_replaced);
    assert_eq!(llm.captures.lock().unwrap().len(), 3);
    assert!(
        outcome
            .transcript
            .iter()
            .all(|message| message.role != ChatRole::Tool)
    );
    assert!(
        outcome
            .transcript
            .iter()
            .all(|message| message.tool_calls.is_empty())
    );
    std::fs::remove_dir_all(config_dir).ok();
}

#[tokio::test]
async fn large_compaction_output_is_accepted_without_repair() {
    let lua = r#"
bone.on("before_turn", function()
    return { conversation = { compact = { instruction = "summarize", keep_recent_turns = 0 } } }
end)
"#;
    let large_checkpoint = "x".repeat(20_000);
    let llm = Arc::new(CompactionProvider::new(vec![
        MockAttempt::Stream(vec![Ok(ChatEvent::TextDelta(large_checkpoint.clone()))]),
        MockAttempt::Stream(vec![Ok(ChatEvent::TextDelta("done".into()))]),
    ]));
    let prompt = "current";
    let transcript = vec![
        ChatMessage::new(ChatRole::User, "old"),
        ChatMessage::new(ChatRole::Assistant, "old answer"),
        ChatMessage::new(ChatRole::User, prompt),
    ];
    let (driver, config_dir) = compaction_test_driver(
        "driver-compaction-large-output",
        lua,
        llm.clone(),
        transcript,
        None,
    );

    let outcome = driver.run_to_outcome(prompt).await;
    assert_eq!(outcome.result.as_ref().unwrap().content, "done");
    assert!(outcome.transcript_replaced);
    assert!(
        outcome
            .transcript
            .iter()
            .any(|message| message.content.contains(&large_checkpoint))
    );
    assert_eq!(llm.captures.lock().unwrap().len(), 2);
    std::fs::remove_dir_all(config_dir).ok();
}

#[tokio::test]
async fn compaction_transport_failure_does_not_repair_or_replace_transcript() {
    let lua = r#"
bone.on("before_turn", function()
    return { conversation = { compact = { instruction = "summarize", keep_recent_turns = 0 } } }
end)
"#;
    let llm = Arc::new(CompactionProvider::new(vec![
        MockAttempt::ConnErr(LlmError::new_with_kind(
            LlmErrorKind::Connection,
            "private failure",
        )),
        MockAttempt::Stream(vec![Ok(ChatEvent::TextDelta("done".into()))]),
    ]));
    let prompt = "current";
    let transcript = vec![
        ChatMessage::new(ChatRole::User, "old"),
        ChatMessage::new(ChatRole::Assistant, "old answer"),
        ChatMessage::new(ChatRole::User, prompt),
    ];
    let original = transcript.clone();
    let (driver, config_dir) = compaction_test_driver(
        "driver-compaction-transport",
        lua,
        llm.clone(),
        transcript,
        None,
    );

    let outcome = driver.run_to_outcome(prompt).await;
    assert_eq!(outcome.result.as_ref().unwrap().content, "done");
    assert!(!outcome.transcript_replaced);
    assert_eq!(llm.captures.lock().unwrap().len(), 2);
    assert_eq!(&outcome.transcript[..original.len()], original.as_slice());
    std::fs::remove_dir_all(config_dir).ok();
}

#[tokio::test]
async fn same_pass_replacement_skips_compaction() {
    let lua = r#"
bone.on("before_turn", function()
    return { conversation = { compact = { instruction = "must not run" } } }
end)
bone.on("before_turn", function()
    return {
        action = "conversation.replace",
        messages = { { role = "user", content = "replacement wins" } },
    }
end)
"#;
    let llm = Arc::new(CompactionProvider::new(vec![MockAttempt::Stream(vec![
        Ok(ChatEvent::TextDelta("done".into())),
    ])]));
    let prompt = "current";
    let transcript = vec![ChatMessage::new(ChatRole::User, prompt)];
    let (driver, config_dir) = compaction_test_driver(
        "driver-compaction-replace",
        lua,
        llm.clone(),
        transcript,
        None,
    );

    let outcome = driver.run_to_outcome(prompt).await;
    assert_eq!(outcome.result.as_ref().unwrap().content, "done");
    assert!(outcome.transcript_replaced);
    assert_eq!(outcome.transcript[0].content, "replacement wins");
    assert_eq!(llm.captures.lock().unwrap().len(), 1);
    std::fs::remove_dir_all(config_dir).ok();
}

#[tokio::test]
async fn compaction_runs_only_once_across_normal_tool_rounds() {
    let lua = r#"
bone.on("before_turn", function()
    return { conversation = { compact = { instruction = "compact once", keep_recent_turns = 0 } } }
end)
"#;
    let llm = Arc::new(CompactionProvider::new(vec![
        MockAttempt::Stream(vec![Ok(ChatEvent::TextDelta("checkpoint once".into()))]),
        MockAttempt::Stream(vec![Ok(ChatEvent::ToolCall(ToolCall {
            id: "normal-call".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({ "path": "Cargo.toml" }),
        }))]),
        MockAttempt::Stream(vec![Ok(ChatEvent::TextDelta("done".into()))]),
    ]));
    let prompt = "current";
    let transcript = vec![
        ChatMessage::new(ChatRole::User, "old"),
        ChatMessage::new(ChatRole::Assistant, "old answer"),
        ChatMessage::new(ChatRole::User, prompt),
    ];
    let (driver, config_dir) = compaction_test_driver(
        "driver-compaction-one-cycle",
        lua,
        llm.clone(),
        transcript,
        None,
    );

    let outcome = driver.run_to_outcome(prompt).await;
    assert_eq!(outcome.result.as_ref().unwrap().content, "done");
    let captures = llm.captures.lock().unwrap();
    assert_eq!(
        captures.len(),
        3,
        "one private request plus two normal rounds"
    );
    assert_eq!(
        captures
            .iter()
            .filter(|capture| capture
                .messages
                .last()
                .is_some_and(|message| message.content.contains("Output only the checkpoint text")))
            .count(),
        1,
        "later before_turn passes must not start another compaction cycle"
    );
    drop(captures);
    assert_eq!(
        outcome
            .transcript
            .iter()
            .filter(|message| message.content.contains("checkpoint once"))
            .count(),
        1
    );
    std::fs::remove_dir_all(config_dir).ok();
}

#[tokio::test]
async fn empty_compaction_and_empty_repair_leave_transcript_unchanged() {
    let lua = r#"
bone.on("before_turn", function()
    return { conversation = { compact = { instruction = "summarize", keep_recent_turns = 0 } } }
end)
"#;
    let llm = Arc::new(CompactionProvider::new(vec![
        MockAttempt::Stream(Vec::new()),
        MockAttempt::Stream(Vec::new()),
        MockAttempt::Stream(vec![Ok(ChatEvent::TextDelta("done".into()))]),
    ]));
    let prompt = "current";
    let transcript = vec![
        ChatMessage::new(ChatRole::User, "old"),
        ChatMessage::new(ChatRole::Assistant, "old answer"),
        ChatMessage::new(ChatRole::User, prompt),
    ];
    let original = transcript.clone();
    let (driver, config_dir) = compaction_test_driver(
        "driver-compaction-empty",
        lua,
        llm.clone(),
        transcript,
        None,
    );

    let outcome = driver.run_to_outcome(prompt).await;
    assert_eq!(outcome.result.as_ref().unwrap().content, "done");
    assert!(!outcome.transcript_replaced);
    assert_eq!(&outcome.transcript[..original.len()], original.as_slice());
    let captures = llm.captures.lock().unwrap();
    assert_eq!(captures.len(), 3);
    assert!(
        captures[1]
            .messages
            .last()
            .unwrap()
            .content
            .contains("only repair attempt")
    );
    drop(captures);
    std::fs::remove_dir_all(config_dir).ok();
}

#[tokio::test]
async fn cancellation_during_private_compaction_does_not_repair_or_replace_transcript() {
    let lua = r#"
bone.on("before_turn", function()
    return { conversation = { compact = { instruction = "summarize", keep_recent_turns = 0 } } }
end)
"#;
    let llm = Arc::new(CompactionProvider::new(vec![MockAttempt::Pending]));
    let prompt = "current";
    let transcript = vec![
        ChatMessage::new(ChatRole::User, "old"),
        ChatMessage::new(ChatRole::Assistant, "old answer"),
        ChatMessage::new(ChatRole::User, prompt),
    ];
    let original = transcript.clone();
    let (mut driver, config_dir) = compaction_test_driver(
        "driver-compaction-cancelled",
        lua,
        llm.clone(),
        transcript,
        None,
    );
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    driver.cancel = Some(cancel.clone());

    let run = tokio::spawn(driver.run_to_outcome(prompt));
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if llm.captures.lock().unwrap().len() == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("private compaction request did not start");
    cancel.store(true, std::sync::atomic::Ordering::Relaxed);

    let outcome = tokio::time::timeout(std::time::Duration::from_secs(1), run)
        .await
        .expect("private compaction did not observe cancellation")
        .expect("driver task panicked");
    assert_eq!(outcome.result.as_ref().unwrap().content, "");
    assert!(!outcome.transcript_replaced);
    assert_eq!(outcome.transcript, original);
    assert_eq!(llm.captures.lock().unwrap().len(), 1);
    std::fs::remove_dir_all(config_dir).ok();
}

// --- Cancellation ---

#[tokio::test]
async fn driver_cancelled_before_turn_returns_empty() {
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (mut driver, prompt) = driver_with(
        vec![ChatEvent::TextDelta("should not appear".into())],
        ApprovalMode::Safe,
    );
    driver.cancel = Some(cancel);

    let outcome = driver.run_to_outcome(prompt).await;
    assert_eq!(outcome.result.unwrap().content, "");
    assert!(
        outcome.transcript.iter().all(|m| m.role == ChatRole::User),
        "no assistant message committed; got {:?}",
        outcome.transcript
    );
}

/// A provider that emits the *same* failing tool call on every request, with
/// no `finish_reason`/usage — the exact shape of a local model stuck spewing a
/// broken edit as fast as it can generate. Used to prove the runaway brake
/// aborts instead of looping forever.
struct RepeatProvider {
    model: String,
    call: ToolCall,
}

#[async_trait]
impl LlmProvider for RepeatProvider {
    fn id(&self) -> &str {
        "repeat"
    }
    fn name(&self) -> &str {
        "Repeat Provider"
    }
    fn model(&self) -> &str {
        &self.model
    }
    fn set_model(&mut self, model: String) {
        self.model = model;
    }
    async fn chat_stream(
        &self,
        _messages: Vec<ChatMessage>,
        _tools: Vec<ToolDefinition>,
    ) -> Result<ResponseStream, LlmError> {
        let events = vec![Ok(ChatEvent::ToolCall(self.call.clone()))];
        Ok(futures_util::stream::iter(events).boxed())
    }
}

/// The runaway brake: a model that resends the identical failing tool call must
/// not loop forever. After a few identical failures the Driver aborts the turn
/// with an error rather than spinning (previously the top-level agent had no
/// tool-error cap at all).
#[tokio::test]
async fn repeated_identical_failing_tool_call_aborts() {
    let prompt = "hi";
    let transcript = vec![ChatMessage::new(ChatRole::User, prompt)];
    let history = build_chat_history(&transcript, None);
    let llm = Arc::new(RepeatProvider {
        model: "repeat-1".into(),
        // A read is auto-allowed in Safe mode, and a missing file fails with a
        // deterministic error every time — so each round's error signature is
        // identical, tripping the brake.
        call: ToolCall {
            id: "call_1".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({ "path": "/nonexistent/bone-runaway-test" }),
        },
    });
    let driver = Driver {
        llm,
        extensions: ExtensionManager::unloaded(),
        tools: ToolHandler::new(builtin_tools()),
        session: Arc::new(NullSessionSink) as Arc<dyn SessionSink>,
        gate: Arc::new(AutoApprovalGate),
        approval_mode: bone_core::tools::SharedApprovalMode::new(ApprovalMode::Safe),
        agent_depth: 0,
        activity: None,
        on_token_usage: None,
        events: false,
        event_sender: None,
        runtime_events: None,
        key_reply_registry: None,
        cancel: None,
        history,
        transcript,
        token_stats: TokenStats::new(),
        system_prompt_override: None,
        conversation_id: None,
        config_store: common::config_store(),
        turn_nudge: Arc::new(std::sync::Mutex::new(None)),
    };

    // Bound the whole thing: if the brake regresses, this loops forever, so the
    // timeout converts a hang into a clear failure.
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        driver.run_to_outcome(prompt),
    )
    .await
    .expect("driver must abort the runaway, not hang");

    let err = match outcome.result {
        Ok(_) => panic!("runaway must abort with an error, not finish ok"),
        Err(e) => e,
    };
    assert!(
        err.contains("in a row"),
        "abort reason should name the repeated-failure loop, got: {err}"
    );
}

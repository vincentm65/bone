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
use bone_core::llm::provider::LlmProvider;
use bone_core::llm::{
    ChatEvent, ChatMessage, ChatRole, LlmError, LlmErrorKind, ResponseStream, TokenStats,
};
use bone_core::runtime::{ChannelApprovalGate, Driver, RuntimeEvent};
use bone_core::session_sink::{NullSessionSink, SessionSink};
use bone_core::tools::registry::ToolHandler;
use bone_core::tools::types::{Tool, ToolExecutionContext, ToolLiveEvent, ToolOutput};
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
    driver.runtime_events = Some(tx);

    let mut conn = LocalConn::new(
        rx,
        driver,
        Arc::new(AtomicBool::new(false)),
        ApprovalReplyRegistry::new(),
        KeyReplyRegistry::new(),
    );
    conn.send(RuntimeCommand::SubmitPrompt {
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
        let persist_from = session.transcript.len();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
        let driver = session.build_driver(
            llm,
            ExtensionManager::unloaded(),
            SharedApprovalMode::new(ApprovalMode::Safe),
            Arc::new(AutoApprovalGate),
            tx,
            KeyReplyRegistry::new(),
            Arc::new(AtomicBool::new(false)),
            Arc::new(NullSessionSink) as Arc<dyn SessionSink>,
        );
        let mut conn = LocalConn::new(
            rx,
            driver,
            Arc::new(AtomicBool::new(false)),
            ApprovalReplyRegistry::new(),
            KeyReplyRegistry::new(),
        );
        conn.send(RuntimeCommand::SubmitPrompt {
            text: prompt.to_string(),
            images: vec![],
        });
        while conn.next_event().await.is_some() {}
        let outcome = conn.take_outcome().expect("turn produced an outcome");
        session
            .apply_outcome(outcome, persist_from)
            .expect("turn ok");
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
    let gate: Arc<dyn ApprovalGate> = Arc::new(ChannelApprovalGate::new(evtx, registry.clone()));
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
        events: Option<tokio::sync::mpsc::UnboundedSender<ToolLiveEvent>>,
        _context: ToolExecutionContext,
    ) -> Result<ToolOutput, String> {
        let Some(tx) = events else {
            return Ok(ToolOutput::text("no events".into()));
        };
        let (reply, rx) = tokio::sync::oneshot::channel();
        tx.send(ToolLiveEvent::Key(bone_core::pane_content::KeyRequest {
            reply,
        }))
        .unwrap();
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

    let mut custom = bone_core::config::custom::CustomConfigs::default();
    let booted = boot_with_tools(
        &config_dir,
        &config_dir,
        &mut custom,
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

    let mut custom = bone_core::config::custom::CustomConfigs::default();
    let booted = boot_with_tools(
        &config_dir,
        &config_dir,
        &mut custom,
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

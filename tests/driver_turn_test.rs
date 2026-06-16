//! Phase 2 acceptance: the core `Driver` runs a full turn headless, with no
//! terminal, no real provider, and no DB — proving the agent loop now lives in
//! one reusable place. Drives the `Driver` directly with a scripted
//! `MockProvider`, `ExtensionManager::unloaded()`, builtin tools, and a
//! `NullSessionSink`, then asserts the emitted `AgentRunEvent` sequence.

use async_trait::async_trait;
use futures_util::StreamExt; // for .boxed()
use std::sync::{Arc, Mutex};

use bone::agent::AgentRunEvent;
use bone::chat::build_chat_history;
use bone::ext::ExtensionManager;
use bone::llm::provider::LlmProvider;
use bone::llm::{ChatEvent, ChatMessage, ChatRole, LlmError, ResponseStream, TokenStats};
use bone::pane_content::{PaneContent, PaneLineSpec};
use bone::runtime::{ChannelApprovalGate, Driver, RuntimeEvent};
use bone::session_sink::{NullSessionSink, SessionSink};
use bone::tools::registry::ToolHandler;
use bone::tools::types::{Tool, ToolExecutionContext, ToolLiveEvent, ToolOutput};
use bone::tools::{
    ApprovalGate, ApprovalMode, AutoApprovalGate, CallOutcome, ToolCall, ToolDefinition,
    builtin_tools,
};

/// Deterministic provider that replays one scripted stream per `chat_stream`
/// call. After the script is drained, subsequent calls yield an empty stream
/// (no text, no tool calls) — which the loop treats as a final empty turn.
struct MockProvider {
    model: String,
    script: Mutex<Vec<ChatEvent>>,
}

impl MockProvider {
    fn new(model: &str, script: Vec<ChatEvent>) -> Self {
        Self {
            model: model.to_string(),
            script: Mutex::new(script),
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
        let events = self.script.lock().unwrap().drain(..).collect::<Vec<_>>();
        Ok(futures_util::stream::iter(events.into_iter().map(Ok)).boxed())
    }
}

fn driver_with(script: Vec<ChatEvent>, mode: ApprovalMode) -> (Driver, &'static str) {
    driver_with_gate(script, mode, Arc::new(AutoApprovalGate))
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
        approval_mode: mode,
        agent_depth: 0,
        activity: None,
        on_token_usage: None,
        events: false,
        event_sender: None,
        runtime_events: None,
        reply_registry: None,
        cancel: None,
        history,
        transcript,
        token_stats: TokenStats::new(),
        system_prompt_override: None,
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

/// Phase: channel-based approval. The Driver consults a `ChannelApprovalGate`
/// for every tool call; the "frontend" (the test) receives an `ApprovalRequest`
/// and replies. Approve → the tool runs (success on a real temp file). Deny →
/// the tool is skipped with an error result, even though Safe mode would have
/// auto-allowed this read-only call (the frontend reply overrides policy).
async fn run_with_channel_decision(label: &str, decision: CallOutcome, expect_error: bool) {
    // A real, readable file so an Approved read_file succeeds (is_error=false).
    let path = std::env::temp_dir().join(format!("bone-approval-test-{label}"));
    std::fs::write(&path, "hello").unwrap();

    let (atx, mut arx) = tokio::sync::mpsc::unbounded_channel::<bone::runtime::ApprovalRequest>();
    let (etx, mut erx) = tokio::sync::mpsc::unbounded_channel::<AgentRunEvent>();
    let gate: Arc<dyn ApprovalGate> = Arc::new(ChannelApprovalGate::new(atx));
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

    // Act as the frontend: receive the approval request and reply.
    let req = tokio::time::timeout(std::time::Duration::from_secs(5), arx.recv())
        .await
        .expect("approval request timed out")
        .expect("approval request");
    assert_eq!(req.call.name, "read_file");
    assert!(req.auto_allows, "read-only in Safe mode is auto-allowed");
    req.reply.send(decision).unwrap();

    let response = run.await.unwrap().expect("driver run");
    assert_eq!(response.content, "", "second (empty) turn finishes");

    // The reply decided whether the tool ran (success on a real file) or was
    // skipped (error result) — proving channel approval overrides auto-allow.
    let mut tool_error: Option<bool> = None;
    while let Ok(ev) = erx.try_recv() {
        if let AgentRunEvent::ToolResult { name, is_error } = ev {
            if name == "read_file" {
                tool_error = Some(is_error);
            }
        }
    }
    assert_eq!(
        tool_error,
        Some(expect_error),
        "tool result error-ness must match the channel decision"
    );

    std::fs::remove_file(&path).ok();
}

/// A tool that emits a live pane update, to prove the Driver forwards
/// `ToolLiveEvent::Pane` to the frontend as `RuntimeEvent::Pane`.
struct PaneTool;

struct InteractTool;

#[async_trait]
impl Tool for PaneTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "pane_tool".into(),
            description: "emits a pane".into(),
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
        if let Some(tx) = events {
            let _ = tx.send(ToolLiveEvent::Pane(PaneContent {
                source: "pane_tool".into(),
                title: "Live".into(),
                lines: vec![PaneLineSpec::Plain("hello from tool".into())],
                visible_rows: 3,
                scroll: 0,
            }));
        }
        Ok(ToolOutput::text("done".into()))
    }
}

#[async_trait]
impl Tool for InteractTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "interact_tool".into(),
            description: "asks a question".into(),
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
        tx.send(ToolLiveEvent::Interact(
            bone::pane_content::InteractRequest {
                question: "pick one".into(),
                mode: bone::pane_content::InteractionMode::SingleSelect,
                options: vec!["yes".into(), "no".into()],
                default_selected: 0,
                allow_custom: false,
                reply,
            },
        ))
        .unwrap();
        let value = rx.await.unwrap();
        Ok(ToolOutput::text(
            value["value"].as_str().unwrap().to_string(),
        ))
    }
}

#[tokio::test]
async fn driver_forwards_tool_pane_to_runtime_event() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
    let prompt = "hi";
    let transcript = vec![ChatMessage::new(ChatRole::User, prompt)];
    let history = build_chat_history(&transcript, None);
    let mut driver = Driver {
        llm: Arc::new(MockProvider::new(
            "mock-1",
            vec![ChatEvent::ToolCall(ToolCall {
                id: "c1".into(),
                name: "pane_tool".into(),
                arguments: serde_json::json!({}),
            })],
        )),
        extensions: ExtensionManager::unloaded(),
        tools: ToolHandler::new(builtin_tools().register(PaneTool)),
        session: Arc::new(NullSessionSink) as Arc<dyn SessionSink>,
        gate: Arc::new(AutoApprovalGate),
        approval_mode: ApprovalMode::Danger, // pane_tool isn't read-only
        agent_depth: 0,
        activity: None,
        on_token_usage: None,
        events: false,
        event_sender: None,
        runtime_events: Some(tx),
        reply_registry: None,
        cancel: None,
        history,
        transcript,
        token_stats: TokenStats::new(),
        system_prompt_override: None,
    };
    let _ = &mut driver;

    driver.run(prompt).await.expect("driver run");

    let mut saw_pane = false;
    while let Ok(ev) = rx.try_recv() {
        if let RuntimeEvent::Pane { pane } = ev {
            if pane.source == "pane_tool" {
                assert!(matches!(&pane.lines[0], PaneLineSpec::Plain(s) if s == "hello from tool"));
                saw_pane = true;
            }
        }
    }
    assert!(
        saw_pane,
        "Driver must forward the tool's ToolLiveEvent::Pane as RuntimeEvent::Pane"
    );
}

#[tokio::test]
async fn driver_interact_reply_completes_turn() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
    let registry = bone::runtime::ReplyRegistry::new();
    let prompt = "hi";
    let transcript = vec![ChatMessage::new(ChatRole::User, prompt)];
    let history = build_chat_history(&transcript, None);
    let driver = Driver {
        llm: Arc::new(MockProvider::new(
            "mock-1",
            vec![ChatEvent::ToolCall(ToolCall {
                id: "c1".into(),
                name: "interact_tool".into(),
                arguments: serde_json::json!({}),
            })],
        )),
        extensions: ExtensionManager::unloaded(),
        tools: ToolHandler::new(builtin_tools().register(InteractTool)),
        session: Arc::new(NullSessionSink) as Arc<dyn SessionSink>,
        gate: Arc::new(AutoApprovalGate),
        approval_mode: ApprovalMode::Danger,
        agent_depth: 0,
        activity: None,
        on_token_usage: None,
        events: false,
        event_sender: None,
        runtime_events: Some(tx),
        reply_registry: Some(registry.clone()),
        cancel: None,
        history,
        transcript,
        token_stats: TokenStats::new(),
        system_prompt_override: None,
    };

    let run = tokio::spawn(async move { driver.run(prompt).await });
    let spec = loop {
        match tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("runtime event timed out")
            .expect("runtime event")
        {
            RuntimeEvent::Interact { spec } => break spec,
            _ => {}
        }
    };
    assert_eq!(spec.question, "pick one");
    assert!(registry.resolve(spec.id, serde_json::json!({ "value": "yes" })));

    tokio::time::timeout(std::time::Duration::from_secs(5), run)
        .await
        .expect("driver wedged after interact reply")
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

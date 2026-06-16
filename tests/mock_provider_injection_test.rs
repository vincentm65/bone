//! Proof-of-viability: a provider can be injected as `Box<dyn LlmProvider>`.
//!
//! This gate exists to confirm that the `LlmProvider` trait (provider.rs:221)
//! is object-safe and externally implementable BEFORE Step 0 of the core/client
//! split, which requires `run_agent` / `App` to *accept* a provider instead of
//! constructing one internally (agent.rs:336-361).
//!
//! If this file compiles and passes, provider injection is viable.

use async_trait::async_trait;
use futures_util::StreamExt; // for .boxed()
use std::sync::{Arc, Mutex};

use bone::llm::{ChatEvent, ChatMessage, LlmError, LlmProvider, ResponseStream};
use bone::tools::{ToolCall, ToolDefinition};

/// A deterministic provider that replays a scripted sequence of ChatEvents
/// on every `chat_stream` call. Modeled after how real providers build their
/// ResponseStream (`Ok(Box::pin(stream))`, openai_compat/mod.rs:530).
struct MockProvider {
    id: &'static str,
    model: String,
    script: Mutex<Vec<ChatEvent>>,
}

impl MockProvider {
    fn new(id: &'static str, model: &str, script: Vec<ChatEvent>) -> Self {
        Self {
            id,
            model: model.to_string(),
            script: Mutex::new(script),
        }
    }
}

#[async_trait]
impl LlmProvider for MockProvider {
    fn id(&self) -> &str {
        self.id
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
        // Drain the scripted events into a stream, identical pattern to real
        // providers except deterministic.
        let events = self.script.lock().unwrap().drain(..).collect::<Vec<_>>();
        let stream = futures_util::stream::iter(events.into_iter().map(Ok));
        Ok(stream.boxed())
    }
    async fn validate(&self) -> Result<(), LlmError> {
        Ok(())
    }
}

#[tokio::test]
async fn mock_provider_is_trait_object_and_streams_script() {
    // The key assertion for injectability: a concrete provider becomes a
    // `Box<dyn LlmProvider>` — exactly what Step 0 will pass into the loop.
    let provider: Box<dyn LlmProvider> = Box::new(MockProvider::new(
        "mock",
        "mock-1",
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
    ));

    // Object-safe dispatch works through the trait.
    assert_eq!(provider.id(), "mock");
    assert_eq!(provider.model(), "mock-1");
    assert!(provider.validate().await.is_ok());

    // chat_stream returns the boxed stream and replays the script in order.
    let mut stream = provider
        .chat_stream(Vec::new(), Vec::new())
        .await
        .expect("mock stream");
    let mut got = Vec::new();
    while let Some(chunk) = stream.next().await {
        match chunk.unwrap() {
            ChatEvent::TextDelta(t) => got.push(format!("text:{t}")),
            ChatEvent::TokenUsage { .. } => got.push("usage".into()),
            _ => {}
        }
    }
    assert_eq!(
        got,
        vec!["text:hello ", "text:world", "usage"],
        "scripted events must replay in order"
    );
}

#[tokio::test]
async fn mock_provider_can_emit_tool_call_event() {
    // Confirms the event variant the split test (Part 3) relies on is reachable
    // through the injected provider: a model requesting an edit_file.
    let provider: Box<dyn LlmProvider> = Box::new(MockProvider::new(
        "mock",
        "mock-1",
        vec![ChatEvent::ToolCall(ToolCall {
            id: "call_1".into(),
            name: "edit_file".into(),
            arguments: serde_json::json!({
                "path": "x", "search": "a", "replace": "b"
            }),
        })],
    ));

    let mut stream = provider
        .chat_stream(Vec::new(), Vec::new())
        .await
        .expect("mock stream");
    match stream.next().await.unwrap().unwrap() {
        ChatEvent::ToolCall(call) => {
            assert_eq!(call.name, "edit_file");
            assert_eq!(call.id, "call_1");
        }
        other => panic!("expected ToolCall event, got {other:?}"),
    }
    assert!(stream.next().await.is_none(), "script exhausted");
}

// ─── Step 0 acceptance tests: resolve_provider injection seam ───────────────
//
// These prove the loop now ACCEPTS a provider instead of always constructing
// one (agent.rs pre-split:336-361). The contract under test:
//   1. An injected provider (request.llm) is reused verbatim.
//   2. No config side-effects run when injecting (no last_provider persistence).
//   3. The returned provider is the SAME allocation (Arc sharing), not a
//      reconstruction — proven by refcount, not just equality of id().

use bone::agent::{AgentRequest, resolve_provider};
use bone::config::custom::CustomConfigs;
use bone::tools::ApprovalMode;

#[test]
fn resolve_provider_uses_injected_provider_and_shares_arc() {
    let injected: Arc<dyn LlmProvider> = Arc::new(MockProvider::new("mock", "mock-1", vec![]));
    assert_eq!(Arc::strong_count(&injected), 1, "baseline refcount");

    let request = AgentRequest {
        prompt: "hi".into(),
        approval_mode: ApprovalMode::Safe,
        provider: Some("local".into()), // would normally construct "local"
        model: None,
        system_prompt: None,
        events: false,
        event_sender: None,
        agent_depth: 0,
        on_token_usage: None,
        activity: None,
        llm: Some(injected.clone()), // now refcount == 2
        session_sink: None,
    };
    let mut custom = CustomConfigs::default();
    let mut pc = custom.derive_providers_config();

    let resolved = resolve_provider(&request, &mut custom, &mut pc).expect("injected must win");

    // Injected identity, not whatever "local" would have produced.
    assert_eq!(resolved.id(), "mock");
    assert_eq!(resolved.model(), "mock-1");
    // Sharing proof: resolve_provider cloned the Arc (refcount 3), it did NOT
    // allocate a fresh provider (which would leave refcount at 2).
    assert_eq!(
        Arc::strong_count(&injected),
        3,
        "provider must be shared, not reconstructed"
    );
}

#[test]
fn resolve_provider_short_circuits_without_any_config() {
    // No provider id AND no last_provider in config → the construct path would
    // return "no provider configured". Injection must bypass that entirely.
    let injected: Arc<dyn LlmProvider> = Arc::new(MockProvider::new("mock", "mock-1", vec![]));
    let request = AgentRequest {
        prompt: "hi".into(),
        approval_mode: ApprovalMode::Safe,
        provider: None,
        model: None,
        system_prompt: None,
        events: false,
        event_sender: None,
        agent_depth: 0,
        on_token_usage: None,
        activity: None,
        llm: Some(injected),
        session_sink: None,
    };
    let mut custom = CustomConfigs::default();
    let mut pc = custom.derive_providers_config();

    let resolved = resolve_provider(&request, &mut custom, &mut pc)
        .expect("injected provider must bypass config lookup");
    assert_eq!(resolved.id(), "mock");
    // No side-effect: last_provider stays empty (set_last_provider never ran).
    assert!(
        custom.get_last_provider().is_empty(),
        "injection must not persist last_provider"
    );
}

use super::{
    AgentRequest, SessionWriter, agent_setup, resolve_provider, session_sink_for_request,
    summarize_call_args,
};
use crate::llm::provider::{ChatMessage, LlmError, LlmProvider, ResponseStream};
use crate::session_db::SessionDb;
use crate::session_sink::SessionSink;
use crate::tools::{ApprovalMode, ToolCall, ToolDefinition};
use async_trait::async_trait;
use std::sync::Arc;

fn nested_request(session_sink: Option<Arc<dyn SessionSink>>) -> AgentRequest {
    AgentRequest {
        prompt: "internal task".into(),
        approval_mode: ApprovalMode::Safe,
        provider: None,
        model: None,
        system_prompt: None,
        events: false,
        event_sender: None,
        agent_depth: 1,
        on_token_usage: None,
        activity: None,
        llm: None,
        session_sink,
        background_scope: None,
        tool_allowlist: None,
        max_tokens: None,
        approval_gate: None,
        transcript: None,
        config_store: Some(crate::config::store::ConfigStore::for_test()),
        cancel: None,
    }
}

#[test]
fn delegated_agents_do_not_open_top_level_conversations() {
    let sink = session_sink_for_request(&nested_request(None), "test", "test");
    assert_eq!(sink.conv_id(), None);
}

#[test]
fn delegated_agents_honor_an_explicit_session_sink() {
    struct Sink;
    impl SessionSink for Sink {
        fn conv_id(&self) -> Option<i64> {
            Some(42)
        }
        fn append_chat_message(&self, _message: &crate::llm::ChatMessage, _seq: i64) {}
        fn record_usage(
            &self,
            _: &str,
            _: &str,
            _: u32,
            _: u32,
            _: Option<u32>,
            _: Option<f64>,
            _: bool,
        ) {
        }
        fn end(&self) {}
    }

    let sink = session_sink_for_request(&nested_request(Some(Arc::new(Sink))), "test", "test");
    assert_eq!(sink.conv_id(), Some(42));
}

#[test]
fn session_writer_persists_tool_error_state() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.db");
    let db = SessionDb::open(&path).unwrap();
    let conv_id = db.create_conversation("test", "test").unwrap();
    let writer = SessionWriter {
        db: std::sync::Mutex::new(Some(db)),
        conv_id: Some(conv_id),
        failures: std::sync::atomic::AtomicU64::new(0),
    };

    let mut message = crate::llm::ChatMessage::new(crate::llm::ChatRole::Tool, "failed");
    message.name = Some("shell".to_string());
    message.tool_call_id = Some("call-1".to_string());
    message.is_error = true;
    writer.append_chat_message(&message, 1);
    drop(writer);

    let messages = SessionDb::open(&path)
        .unwrap()
        .load_messages(conv_id)
        .unwrap();
    assert!(messages[0].is_error);
}

#[test]
fn summarize_call_args_truncates_json_on_char_boundary() {
    let value = format!("{}{}{}", "a".repeat(67), "😀", "b".repeat(20));
    let call = ToolCall {
        id: "call_1".to_string(),
        name: "custom_tool".to_string(),
        arguments: serde_json::json!({ "text": value }),
    };

    let summary = summarize_call_args(&call);

    assert!(summary.ends_with("..."));
    assert!(summary.len() <= 80);
}

struct MockProvider;

#[async_trait]
impl LlmProvider for MockProvider {
    fn id(&self) -> &str {
        "mock"
    }
    fn name(&self) -> &str {
        "Mock"
    }
    fn model(&self) -> &str {
        "mock-1"
    }
    fn set_model(&mut self, _model: String) {}

    async fn chat_stream(
        &self,
        _messages: Vec<ChatMessage>,
        _tools: Vec<ToolDefinition>,
    ) -> Result<ResponseStream, LlmError> {
        Ok(Box::pin(futures_util::stream::empty()))
    }
}

fn request(llm: Arc<dyn LlmProvider>) -> AgentRequest {
    AgentRequest {
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
        llm: Some(llm),
        session_sink: None,
        background_scope: None,
        tool_allowlist: None,
        max_tokens: None,
        approval_gate: None,
        transcript: None,
        cancel: None,
        config_store: None,
    }
}

fn request_with_config(
    llm: Arc<dyn LlmProvider>,
    config: crate::config::store::ConfigStore,
) -> AgentRequest {
    let mut req = request(llm);
    req.config_store = Some(config);
    req.session_sink = Some(Arc::new(crate::session_sink::NullSessionSink));
    req
}

/// Run `f` with `BONE_DIR` pointed at a fresh tempdir, restoring the prior
/// value (or removing the variable) on drop.
fn with_bone_dir(f: impl FnOnce(tempfile::TempDir)) {
    let _guard = crate::util::test_env_lock();
    let previous = std::env::var_os("BONE_DIR");
    let dir = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("BONE_DIR", dir.path()) };
    f(dir);
    unsafe {
        match previous {
            Some(value) => std::env::set_var("BONE_DIR", value),
            None => std::env::remove_var("BONE_DIR"),
        }
    }
}

fn resolve(request: &AgentRequest) -> Result<Arc<dyn LlmProvider>, String> {
    let mut providers = crate::config::ProvidersConfig::default();
    resolve_provider(request, None, &mut providers)
}

#[test]
fn reuses_injected_provider_arc() {
    let injected: Arc<dyn LlmProvider> = Arc::new(MockProvider);
    let request = request(injected.clone());
    let resolved = resolve(&request).unwrap();
    assert!(Arc::ptr_eq(&injected, &resolved));
}

#[test]
fn injection_bypasses_provider_config_without_side_effects() {
    let mut providers = crate::config::ProvidersConfig::default();
    let request = request(Arc::new(MockProvider));
    let resolved = resolve_provider(&request, None, &mut providers).unwrap();
    assert_eq!(resolved.id(), "mock");
    assert!(providers.last_provider.is_empty());
}

#[test]
fn injection_rejects_max_tokens() {
    let mut request = request(Arc::new(MockProvider));
    request.max_tokens = Some(1);
    assert_eq!(
        resolve(&request).err().as_deref(),
        Some("max_tokens is not supported with an injected provider")
    );
}

#[test]
fn headless_prompt_overrides_ignore_the_daemon_main_prompt() {
    with_bone_dir(|_dir| {
        let config = crate::config::store::ConfigStore::for_test();
        let revision = config.snapshot().revision;
        config
            .set_value(
                "general.system_prompt",
                serde_json::json!("Daemon main-agent prompt"),
                revision,
            )
            .unwrap();

        let top_level = {
            let mut req = request_with_config(Arc::new(MockProvider), config.clone());
            req.system_prompt = Some("Explicit bone run prompt".into());
            agent_setup(&req).unwrap()
        };
        assert_eq!(
            top_level.system_prompt_override.as_deref(),
            Some("Explicit bone run prompt")
        );
        assert_eq!(top_level.history[0].content, "Explicit bone run prompt");

        let mut delegated = request_with_config(Arc::new(MockProvider), config);
        delegated.agent_depth = 1;
        delegated.system_prompt = Some("Delegated persona".into());
        let delegated = agent_setup(&delegated).unwrap();
        let delegated_prompt = delegated.system_prompt_override.unwrap();
        assert!(delegated_prompt.starts_with("Delegated persona\n\nRules:"));
        assert!(delegated_prompt.contains("You run non-interactively"));
        assert!(!delegated_prompt.contains("Daemon main-agent prompt"));
        assert_eq!(delegated.history[0].content, delegated_prompt);
    });
}

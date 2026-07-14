use super::{AgentRequest, session_sink_for_request, summarize_call_args};
use crate::session_sink::SessionSink;
use crate::tools::ApprovalMode;
use crate::tools::ToolCall;
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
        tool_allowlist: None,
        max_tokens: None,
        approval_gate: None,
        transcript: None,
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
        fn append_message(
            &self,
            _: &str,
            _: &str,
            _: Option<&str>,
            _: Option<&str>,
            _: Option<&str>,
            _: Option<&str>,
            _: i64,
        ) {
        }
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

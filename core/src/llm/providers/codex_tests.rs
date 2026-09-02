use super::{
    CodexProvider, CodexResponse, codex_request_identity, extract_response_events, output_index,
    process_summary_event,
};
use crate::llm::provider::{ChatEvent, ProviderRequestContext};
use crate::tools::TRUNCATED_ARGS_KEY;
use serde_json::json;
use std::collections::BTreeSet;

#[test]
fn conversation_identity_takes_precedence_over_cache_scope() {
    let context = ProviderRequestContext {
        conversation_id: Some(42),
        cache_scope: Some("run-ignored".into()),
        ..Default::default()
    };

    assert_eq!(
        codex_request_identity(&context).as_deref(),
        Some("00000000-0000-4000-8000-00000000002a")
    );
}

#[test]
fn cache_scope_identity_is_stable_and_uuid_shaped() {
    let context = ProviderRequestContext {
        cache_scope: Some("delegated-run".into()),
        ..Default::default()
    };
    let identity = codex_request_identity(&context).unwrap();

    assert_eq!(identity, codex_request_identity(&context).unwrap());
    assert_eq!(identity.len(), 36);
    assert_eq!(
        identity
            .char_indices()
            .filter_map(|(index, ch)| (ch == '-').then_some(index))
            .collect::<Vec<_>>(),
        vec![8, 13, 18, 23]
    );
}

#[test]
fn different_cache_scopes_have_different_identities() {
    let first = ProviderRequestContext {
        cache_scope: Some("run-1".into()),
        ..Default::default()
    };
    let second = ProviderRequestContext {
        cache_scope: Some("run-2".into()),
        ..Default::default()
    };

    assert_ne!(
        codex_request_identity(&first),
        codex_request_identity(&second)
    );
}

#[test]
fn missing_conversation_and_scope_has_no_identity() {
    assert_eq!(
        codex_request_identity(&ProviderRequestContext::default()),
        None
    );
}

#[test]
fn fast_mode_maps_to_priority_service_tier() {
    let enabled = serde_yaml::from_str("handler: codex\nfast_mode: true\n").unwrap();
    assert_eq!(
        CodexProvider::from_entry("codex", &enabled).service_tier(),
        Some("priority")
    );

    let disabled = serde_yaml::from_str("handler: codex\n").unwrap();
    assert_eq!(
        CodexProvider::from_entry("codex", &disabled).service_tier(),
        None
    );
}

#[test]
fn completed_tool_calls_follow_argument_contract() {
    let response: CodexResponse = serde_json::from_value(json!({
        "output": [
            {"type": "function_call", "call_id": "ok", "name": "tool", "arguments": "  "},
            {"type": "function_call", "call_id": "bad", "name": "tool", "arguments": "{\"x\":"},
            {"type": "function_call", "call_id": "", "name": "tool", "arguments": "{}"}
        ]
    }))
    .unwrap();
    let (events, _) = extract_response_events(&response, &Default::default());
    assert_eq!(events.len(), 2);
    assert!(matches!(&events[0], ChatEvent::ToolCall(call) if call.arguments == json!({})));
    assert!(
        matches!(&events[1], ChatEvent::ToolCall(call) if call.arguments[TRUNCATED_ARGS_KEY] == "{\"x\":")
    );
}

#[test]
fn streamed_summary_done_does_not_duplicate_deltas() {
    let mut emitted = BTreeSet::new();
    let delta = json!({"output_index": 2, "summary_index": 1, "delta": "Checked "});
    let done = json!({"output_index": 2, "summary_index": 1, "text": "Checked it."});

    assert!(matches!(
        process_summary_event(
            "response.reasoning_summary_text.delta",
            &delta,
            &mut emitted
        ),
        Some(ChatEvent::ReasoningDelta { text, echo_field: None }) if text == "Checked "
    ));
    assert!(
        process_summary_event("response.reasoning_summary_text.done", &done, &mut emitted)
            .is_none()
    );
}

#[test]
fn streamed_summary_done_is_fallback_without_deltas() {
    let mut emitted = BTreeSet::new();
    let done = json!({"output_index": 0, "summary_index": 0, "text": "Checked it."});

    assert!(matches!(
        process_summary_event(
            "response.reasoning_summary_text.done",
            &done,
            &mut emitted
        ),
        Some(ChatEvent::ReasoningDelta { text, echo_field: None }) if text == "Checked it."
    ));
}

#[test]
fn completed_reasoning_emits_summary_and_encrypted_replay() {
    let response: CodexResponse = serde_json::from_value(json!({
        "output": [{
            "type": "reasoning",
            "id": "rs_1",
            "summary": [{"type": "summary_text", "text": "Checked the implementation."}],
            "encrypted_content": "encrypted"
        }]
    }))
    .unwrap();

    let (events, _) = extract_response_events(&response, &Default::default());
    assert!(matches!(
        &events[0],
        ChatEvent::ReasoningDelta { text, echo_field: None }
            if text == "Checked the implementation."
    ));
    assert!(matches!(
        &events[1],
        ChatEvent::EncryptedReasoning { id, encrypted_content }
            if id == "rs_1" && encrypted_content == "encrypted"
    ));
}

#[test]
fn completed_reasoning_skips_already_streamed_summary_part() {
    let response: CodexResponse = serde_json::from_value(json!({
        "output": [{
            "type": "reasoning",
            "summary": [{"type": "summary_text", "text": "duplicate"}]
        }]
    }))
    .unwrap();

    let emitted = BTreeSet::from([(0, 0)]);
    let (events, _) = extract_response_events(&response, &emitted);
    assert!(events.is_empty());
}

#[test]
fn output_index_defaults_to_zero() {
    assert_eq!(output_index(&json!({})), 0);
    assert_eq!(output_index(&json!({"output_index": 3})), 3);
}

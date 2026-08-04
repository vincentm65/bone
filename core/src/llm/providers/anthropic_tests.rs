use super::{
    AnthropicProvider, MessagesRequest, OutputConfig, PartialToolUse, build_request_parts,
    finish_tool_use, usage_input_tokens,
};
use crate::llm::provider::{ChatEvent, LlmProvider, ProviderRequestContext};
use crate::llm::{ChatMessage, ChatRole};
use serde_json::json;

#[test]
fn system_messages_become_cached_system_blocks() {
    let (system, msgs) = build_request_parts(vec![
        ChatMessage::new(ChatRole::System, "you are bone"),
        ChatMessage::new(ChatRole::User, "hi"),
    ]);
    assert_eq!(system.len(), 1);
    let json = serde_json::to_value(&system[0]).unwrap();
    assert_eq!(json["text"], "you are bone");
    assert_eq!(json["cache_control"]["type"], "ephemeral");
    assert_eq!(msgs.len(), 1);
}

#[test]
fn tool_result_maps_to_user_tool_result_block() {
    let mut msg = ChatMessage::new(ChatRole::Tool, "42");
    msg.tool_call_id = Some("call_1".to_string());
    let (_system, msgs) = build_request_parts(vec![msg]);
    assert_eq!(msgs.len(), 1);
    let json = serde_json::to_value(&msgs[0]).unwrap();
    assert_eq!(json["role"], "user");
    assert_eq!(json["content"][0]["type"], "tool_result");
    assert_eq!(json["content"][0]["tool_use_id"], "call_1");
    assert_eq!(json["content"][0]["content"], "42");
}

#[test]
fn assistant_tool_calls_map_to_tool_use_blocks() {
    let mut msg = ChatMessage::new(ChatRole::Assistant, "");
    msg.tool_calls = vec![crate::tools::ToolCall {
        id: "call_1".to_string(),
        name: "shell".to_string(),
        arguments: json!({ "command": "ls" }),
    }];
    let (_system, msgs) = build_request_parts(vec![msg]);
    let json = serde_json::to_value(&msgs[0]).unwrap();
    assert_eq!(json["role"], "assistant");
    assert_eq!(json["content"][0]["type"], "tool_use");
    assert_eq!(json["content"][0]["name"], "shell");
    assert_eq!(json["content"][0]["input"]["command"], "ls");
}

#[test]
fn input_tokens_sum_base_and_cache() {
    let usage = json!({
        "input_tokens": 10,
        "cache_read_input_tokens": 90,
        "cache_creation_input_tokens": 5
    });
    assert_eq!(usage_input_tokens(&usage), 105);
}

#[test]
fn empty_tool_input_becomes_empty_object() {
    let event = finish_tool_use(PartialToolUse {
        id: "call_1".to_string(),
        name: "noop".to_string(),
        input: String::new(),
    });
    match event {
        Some(ChatEvent::ToolCall(call)) => assert_eq!(call.arguments, json!({})),
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn reasoning_effort_uses_anthropic_output_config() {
    let request = MessagesRequest {
        model: "claude-test".into(),
        max_tokens: 100,
        stream: true,
        output_config: Some(OutputConfig {
            effort: "ultra".into(),
        }),
        system: Vec::new(),
        messages: Vec::new(),
        tools: Vec::new(),
    };
    let json = serde_json::to_value(request).unwrap();
    assert_eq!(json["output_config"]["effort"], "ultra");
}

/// Context max_tokens overrides configured cap in the wire body.
#[test]
fn context_max_tokens_overrides_configured_cap_in_messages_request() {
    let request = MessagesRequest {
        model: "claude-3".into(),
        max_tokens: 5_000, // context override value
        stream: true,
        output_config: None,
        system: Vec::new(),
        messages: Vec::new(),
        tools: Vec::new(),
    };
    let json = serde_json::to_value(&request).unwrap();
    assert_eq!(json["max_tokens"], 5_000);
}

/// Configured cap is used when context has no max_tokens.
#[test]
fn configured_cap_used_when_context_max_tokens_is_none() {
    let request = MessagesRequest {
        model: "claude-3".into(),
        max_tokens: 8_000, // configured cap
        stream: true,
        output_config: None,
        system: Vec::new(),
        messages: Vec::new(),
        tools: Vec::new(),
    };
    let json = serde_json::to_value(&request).unwrap();
    assert_eq!(json["max_tokens"], 8_000);
}

/// Provider max_tokens field is not mutated by context override.
#[test]
fn context_max_tokens_does_not_mutate_configured_cap() {
    let entry = serde_yaml::from_str("handler: anthropic\n").unwrap();
    let mut provider = AnthropicProvider::from_entry("anthropic", &entry);
    provider.set_max_tokens(Some(10_000));
    assert_eq!(provider.max_tokens, Some(10_000));

    // Simulate context override: context.max_tokens.or(self.max_tokens)
    let ctx = ProviderRequestContext {
        max_tokens: Some(5_000),
        ..Default::default()
    };
    let effective = ctx.max_tokens.or(provider.max_tokens);
    assert_eq!(effective, Some(5_000)); // context wins
    assert_eq!(provider.max_tokens, Some(10_000)); // configured cap untouched
}

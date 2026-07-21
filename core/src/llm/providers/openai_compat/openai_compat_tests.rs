use super::{
    ChatEvent, ChatRequest, OpenAiCompatProvider, ThinkParser, cached_tokens_from_usage,
    flush_stream_end, openai_messages, openai_tools, process_sse_chunk, split_reasoning_events,
    stream_usage_enabled,
};
use crate::llm::{ChatMessage, ChatRole, ImageData};
use std::collections::BTreeMap;

#[test]
fn reads_openai_style_nested_cached_tokens() {
    // OpenAI / GLM nest cache hits under prompt_tokens_details.
    let usage = serde_json::json!({
        "prompt_tokens": 100,
        "prompt_tokens_details": { "cached_tokens": 64 }
    });
    assert_eq!(cached_tokens_from_usage(&usage), Some(64));
}

#[test]
fn reads_deepseek_style_top_level_cache_hit_tokens() {
    // DeepSeek reports cache hits as a top-level usage field.
    let usage = serde_json::json!({
        "prompt_tokens": 100,
        "prompt_cache_hit_tokens": 96,
        "prompt_cache_miss_tokens": 4
    });
    assert_eq!(cached_tokens_from_usage(&usage), Some(96));
}

#[test]
fn no_cache_field_yields_none() {
    let usage = serde_json::json!({ "prompt_tokens": 100 });
    assert_eq!(cached_tokens_from_usage(&usage), None);
}

#[test]
fn stream_end_emits_captured_usage_once() {
    let mut partial_tool_calls = BTreeMap::new();
    let mut usage = Some(serde_json::json!({
        "prompt_tokens": 42,
        "completion_tokens": 7,
        "prompt_cache_hit_tokens": 30,
        "cost": 0.001,
    }));

    let events = flush_stream_end(&mut partial_tool_calls, &mut usage);
    assert!(usage.is_none());
    assert!(matches!(
        events.as_slice(),
        [ChatEvent::TokenUsage {
            prompt_tokens: 42,
            completion_tokens: 7,
            cached_tokens: Some(30),
            cost: Some(cost),
        }] if (*cost - 0.001).abs() < f64::EPSILON
    ));

    assert!(flush_stream_end(&mut partial_tool_calls, &mut usage).is_empty());
}

#[test]
fn requests_stream_usage_from_grok_proxy() {
    assert!(stream_usage_enabled("https://cli-chat-proxy.grok.com/v1"));
}

fn reasoning_events(data: serde_json::Value, think: &mut ThinkParser) -> Vec<ChatEvent> {
    let mut calls = BTreeMap::new();
    let mut usage = None;
    let events = process_sse_chunk(&data.to_string(), &mut calls, &mut usage).unwrap();
    split_reasoning_events(events, think)
}

#[test]
fn handles_inline_and_dedicated_reasoning_without_duplication() {
    let mut think = ThinkParser::new();
    let inline = reasoning_events(
        serde_json::json!({"choices": [{"delta": {"content": "<think>why</think>answer"}}]}),
        &mut think,
    );
    assert!(
        matches!(&inline[..], [ChatEvent::TextDelta(text), ChatEvent::ReasoningDelta { text: reason, .. }] if text == "answer" && reason == "why")
    );

    for key in ["reasoning_content", "thoughts"] {
        let mut delta = serde_json::Map::new();
        delta.insert(
            "content".into(),
            serde_json::json!("<think>why</think>answer"),
        );
        delta.insert(key.into(), serde_json::json!("why"));
        let events = reasoning_events(
            serde_json::json!({"choices": [{"delta": delta}]}),
            &mut ThinkParser::new(),
        );
        assert!(
            matches!(&events[..], [ChatEvent::TextDelta(text), ChatEvent::ReasoningDelta { text: reason, echo_field: Some(field) }] if text.contains("<think>") && reason == "why" && field == key)
        );
    }
}

#[test]
fn handles_think_tags_split_across_chunks() {
    let mut think = ThinkParser::new();
    assert_eq!(think.feed("<thi"), (String::new(), String::new()));
    assert_eq!(think.feed("nk>why</th"), (String::new(), "why".into()));
    assert_eq!(think.feed("ink>answer"), ("answer".into(), String::new()));
}

#[test]
fn ignores_content_in_same_chunk_as_tool_call_delta() {
    let mut partial_tool_calls = BTreeMap::new();
    let mut usage = None;
    let first = r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"shell","arguments":"{\"command\":"}}]}}]}"#;
    let stray = r#"{"choices":[{"delta":{"content":"]","tool_calls":[{"index":0,"function":{"arguments":"\"echo hi\"}"}}]}}]}"#;
    let done = r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#;

    assert!(
        process_sse_chunk(first, &mut partial_tool_calls, &mut usage)
            .unwrap()
            .is_empty()
    );
    assert!(
        process_sse_chunk(stray, &mut partial_tool_calls, &mut usage)
            .unwrap()
            .is_empty()
    );
    let events = process_sse_chunk(done, &mut partial_tool_calls, &mut usage).unwrap();

    assert_eq!(events.len(), 1);
    match &events[0] {
        ChatEvent::ToolCall(call) => {
            assert_eq!(call.name, "shell");
            assert_eq!(call.arguments["command"], "echo hi");
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn wraps_truncated_tool_arguments_in_valid_marker_object() {
    // The model's argument JSON is cut off at the output-token cap, so the
    // stream finishes with an unclosed object. It must surface as a *valid*
    // object keyed by `TRUNCATED_ARGS_KEY` (not `Null`, not a bare string):
    // the message is persisted and re-serialized, so it has to stay valid
    // JSON while still letting the tool validator report truncation.
    let mut partial_tool_calls = BTreeMap::new();
    let mut usage = None;
    let start = r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"edit_file","arguments":"{\"path\":\"world.rs\",\"content\":\"use std"}}]}}]}"#;
    let done = r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#;

    assert!(
        process_sse_chunk(start, &mut partial_tool_calls, &mut usage)
            .unwrap()
            .is_empty()
    );
    let events = process_sse_chunk(done, &mut partial_tool_calls, &mut usage).unwrap();

    assert_eq!(events.len(), 1);
    match &events[0] {
        ChatEvent::ToolCall(call) => {
            assert_eq!(call.name, "edit_file");
            let raw = call.arguments[crate::tools::TRUNCATED_ARGS_KEY]
                .as_str()
                .expect("truncated args wrapped under the marker key");
            assert!(raw.starts_with("{\"path\""));
            // Re-serializing the persisted assistant message must stay valid
            // JSON — never a double-encoded bare string.
            let serialized = call.arguments.to_string();
            assert!(serde_json::from_str::<serde_json::Value>(&serialized).is_ok());
            assert!(serialized.starts_with('{'));
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn serializes_images_as_openai_content_parts() {
    let messages = openai_messages(vec![ChatMessage::user_with_images(
        "look",
        vec![ImageData {
            media_type: "image/png".to_string(),
            data: "abc".to_string(),
        }],
    )]);
    let json = serde_json::to_value(&messages[0]).unwrap();

    assert_eq!(json["content"][0]["type"], "text");
    assert_eq!(json["content"][0]["text"], "look");
    assert_eq!(json["content"][1]["type"], "image_url");
    assert_eq!(
        json["content"][1]["image_url"]["url"],
        "data:image/png;base64,abc"
    );
}

#[test]
fn serializes_text_only_as_plain_string() {
    let messages = openai_messages(vec![ChatMessage::new(ChatRole::User, "hello")]);
    let json = serde_json::to_value(&messages[0]).unwrap();
    assert_eq!(json["content"], "hello");
}

#[test]
fn serializes_tools_with_required_function_envelope() {
    let tools = openai_tools(vec![crate::tools::ToolDefinition {
        name: "shell".into(),
        description: "Run a command".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {"command": {"type": "string"}}
        }),
    }]);
    let json = serde_json::to_value(tools).unwrap();

    assert_eq!(json[0]["type"], "function");
    assert_eq!(json[0]["function"]["name"], "shell");
    assert!(json[0].get("name").is_none());
}

#[test]
fn omits_reasoning_effort_when_unset() {
    let request = ChatRequest {
        model: "grok-build".into(),
        messages: vec![],
        stream: true,
        tools: vec![],
        stream_options: None,
        max_tokens: None,
        reasoning_effort: None,
    };
    let json = serde_json::to_value(&request).unwrap();
    assert!(json.get("reasoning_effort").is_none());
}

#[test]
fn serializes_reasoning_effort_when_set() {
    let request = ChatRequest {
        model: "grok-build".into(),
        messages: vec![],
        stream: true,
        tools: vec![],
        stream_options: None,
        max_tokens: None,
        reasoning_effort: Some("high".into()),
    };
    let json = serde_json::to_value(&request).unwrap();
    assert_eq!(json["reasoning_effort"], "high");
}

#[test]
fn from_entry_reads_reasoning_effort() {
    use crate::config::ProviderEntry;
    let entry = ProviderEntry {
        label: "Grok".into(),
        base_url: "https://example.com/v1".into(),
        model: "grok-build".into(),
        api_key: Default::default(),
        endpoint: "/chat/completions".into(),
        handler: "openai".into(),
        context_window_tokens: Some(131_072),
        reasoning_effort: "HIGH".into(),
    };
    let provider = OpenAiCompatProvider::from_entry("grok", &entry);
    assert_eq!(provider.reasoning_effort.as_deref(), Some("high"));

    let empty = ProviderEntry {
        reasoning_effort: "default".into(),
        ..entry
    };
    let provider = OpenAiCompatProvider::from_entry("grok", &empty);
    assert_eq!(provider.reasoning_effort, None);
}

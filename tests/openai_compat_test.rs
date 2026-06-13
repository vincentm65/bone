use std::collections::BTreeMap;

use bone::llm::ChatEvent;
use bone::llm::providers::openai_compat::{
    PartialToolCall, ThinkParser, delta_has_reasoning_field, flush_partial_tool_calls,
    process_sse_chunk,
};
use serde_json::Value;

/// Helper: build a minimal SSE chunk JSON string with a text content delta.
fn chunk_with_content(content: &str) -> String {
    serde_json::json!({
        "choices": [{
            "delta": {
                "content": content
            }
        }]
    })
    .to_string()
}

/// Helper: build an SSE chunk with usage data.
fn chunk_with_usage(prompt: u32, completion: u32) -> String {
    serde_json::json!({
        "choices": [{
            "delta": {}
        }],
        "usage": {
            "prompt_tokens": prompt,
            "completion_tokens": completion
        }
    })
    .to_string()
}

/// Helper: build an SSE chunk for a tool call delta (partial).
fn chunk_with_tool(index: usize, id: &str, name: &str, arguments: &str) -> String {
    serde_json::json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": index,
                    "id": id,
                    "function": {
                        "name": name,
                        "arguments": arguments
                    }
                }]
            }
        }]
    })
    .to_string()
}

/// Helper: build an SSE chunk with `finish_reason: "tool_calls"`.
fn chunk_tool_finish() -> String {
    serde_json::json!({
        "choices": [{
            "delta": {},
            "finish_reason": "tool_calls"
        }]
    })
    .to_string()
}

#[test]
fn test_text_streaming_across_chunks() {
    let mut partials = BTreeMap::new();
    let mut last_usage = None;

    let events1 = process_sse_chunk(
        &chunk_with_content("Hello "),
        &mut partials,
        &mut last_usage,
    )
    .unwrap();
    assert_eq!(events1.len(), 1);
    assert!(matches!(events1[0], ChatEvent::TextDelta(ref s) if s == "Hello "));

    let events2 = process_sse_chunk(
        &chunk_with_content("world!"),
        &mut partials,
        &mut last_usage,
    )
    .unwrap();
    assert_eq!(events2.len(), 1);
    assert!(matches!(events2[0], ChatEvent::TextDelta(ref s) if s == "world!"));
}

#[test]
fn test_usage_chunk_updates_last_usage() {
    let mut partials = BTreeMap::new();
    let mut last_usage: Option<Value> = None;

    let events =
        process_sse_chunk(&chunk_with_usage(42, 7), &mut partials, &mut last_usage).unwrap();
    assert!(events.is_empty());

    let usage = last_usage.expect("last_usage should be set");
    assert_eq!(usage["prompt_tokens"].as_u64().unwrap(), 42);
    assert_eq!(usage["completion_tokens"].as_u64().unwrap(), 7);
}

#[test]
fn test_single_tool_call_split_across_chunks() {
    let mut partials = BTreeMap::new();
    let mut last_usage = None;

    let e1 = process_sse_chunk(
        &chunk_with_tool(0, "call_", "read_", "{\"pat"),
        &mut partials,
        &mut last_usage,
    )
    .unwrap();
    assert!(e1.is_empty(), "partials shouldn't emit until finish_reason");

    let e2 = process_sse_chunk(
        &chunk_with_tool(0, "", "", "h\": \"/e"),
        &mut partials,
        &mut last_usage,
    )
    .unwrap();
    assert!(e2.is_empty());

    let e3 = process_sse_chunk(
        &chunk_with_tool(0, "", "", "tc\"}"),
        &mut partials,
        &mut last_usage,
    )
    .unwrap();
    assert!(e3.is_empty());

    let e4 = process_sse_chunk(&chunk_tool_finish(), &mut partials, &mut last_usage).unwrap();
    assert_eq!(e4.len(), 1);
    match &e4[0] {
        ChatEvent::ToolCall(call) => {
            assert_eq!(call.id, "call_");
            assert_eq!(call.name, "read_");
            assert_eq!(call.arguments, serde_json::json!({"path": "/etc"}));
        }
        other => panic!("expected ToolCall, got {other:?}"),
    }

    assert!(partials.is_empty());
}

#[test]
fn test_multiple_tool_calls_interleaved_by_index() {
    let mut partials = BTreeMap::new();
    let mut last_usage = None;

    process_sse_chunk(
        &chunk_with_tool(0, "id0", "shell", "{\"cmd\": \"ls\"}"),
        &mut partials,
        &mut last_usage,
    )
    .unwrap();

    process_sse_chunk(
        &chunk_with_tool(1, "id1", "read", "{\"path\": \"/tmp\"}"),
        &mut partials,
        &mut last_usage,
    )
    .unwrap();

    process_sse_chunk(
        &chunk_with_tool(0, "", "", ""),
        &mut partials,
        &mut last_usage,
    )
    .unwrap();

    process_sse_chunk(
        &chunk_with_tool(1, "", "", ""),
        &mut partials,
        &mut last_usage,
    )
    .unwrap();

    assert_eq!(partials.len(), 2);

    let events = process_sse_chunk(&chunk_tool_finish(), &mut partials, &mut last_usage).unwrap();
    assert_eq!(events.len(), 2, "should emit 2 completed tool calls");

    let ids: Vec<&str> = events
        .iter()
        .map(|e| match e {
            ChatEvent::ToolCall(c) => c.id.as_str(),
            _ => "",
        })
        .collect();
    assert!(ids.contains(&"id0"));
    assert!(ids.contains(&"id1"));

    assert!(partials.is_empty());
}

#[test]
fn test_done_flushes_partial_tool_calls() {
    let mut partials = BTreeMap::new();

    let partial = PartialToolCall {
        id: "tool_1".to_string(),
        name: "search".to_string(),
        arguments: "{\"query\": \"hello\"}".to_string(),
    };
    partials.insert(0, partial);

    let events = flush_partial_tool_calls(&mut partials);
    assert_eq!(events.len(), 1);
    match &events[0] {
        ChatEvent::ToolCall(call) => {
            assert_eq!(call.id, "tool_1");
            assert_eq!(call.name, "search");
            assert_eq!(call.arguments, serde_json::json!({"query": "hello"}));
        }
        other => panic!("expected ToolCall, got {other:?}"),
    }

    assert!(partials.is_empty());
}

#[test]
fn test_stream_ends_without_done_still_flushes() {
    let mut partials = BTreeMap::new();

    let partial = PartialToolCall {
        id: "orphan_id".to_string(),
        name: "orphan_tool".to_string(),
        arguments: "{\"x\": 1}".to_string(),
    };
    partials.insert(0, partial);

    let events = flush_partial_tool_calls(&mut partials);
    assert_eq!(events.len(), 1);
    assert!(matches!(&events[0], ChatEvent::ToolCall(c) if c.id == "orphan_id"));

    assert!(partials.is_empty());
}

#[test]
fn test_malformed_tool_arguments_becomes_null() {
    let mut partials = BTreeMap::new();

    let partial = PartialToolCall {
        id: "bad".to_string(),
        name: "broken".to_string(),
        arguments: "this is not json at all!!!".to_string(),
    };
    partials.insert(0, partial);

    let events = flush_partial_tool_calls(&mut partials);
    assert_eq!(events.len(), 1);
    match &events[0] {
        ChatEvent::ToolCall(call) => {
            assert_eq!(call.arguments, Value::Null);
        }
        other => panic!("expected ToolCall, got {other:?}"),
    }
}

#[test]
fn test_empty_partial_skipped_on_flush() {
    let mut partials = BTreeMap::new();
    partials.insert(0, PartialToolCall::default());

    let events = flush_partial_tool_calls(&mut partials);
    assert!(events.is_empty());
}

#[test]
fn test_done_emits_token_usage() {
    let usage_json = serde_json::json!({
        "prompt_tokens": 100,
        "completion_tokens": 50
    });
    let prompt_tokens = usage_json["prompt_tokens"].as_u64().unwrap_or(0) as u32;
    let completion_tokens = usage_json["completion_tokens"].as_u64().unwrap_or(0) as u32;

    assert_eq!(prompt_tokens, 100);
    assert_eq!(completion_tokens, 50);

    let event = ChatEvent::TokenUsage {
        prompt_tokens,
        completion_tokens,
        cached_tokens: None,
        cost: None,
    };
    match event {
        ChatEvent::TokenUsage {
            prompt_tokens: p,
            completion_tokens: c,
            ..
        } => {
            assert_eq!(p, 100);
            assert_eq!(c, 50);
        }
        _ => panic!("expected TokenUsage"),
    }
}

#[test]
fn test_sse_comment_is_skipped() {
    let mut partials = BTreeMap::new();
    let mut last_usage = None;
    let result = process_sse_chunk(": this is a comment", &mut partials, &mut last_usage);
    assert!(result.is_err(), "comment line is not valid JSON");
}

#[test]
fn test_text_then_usage() {
    let mut partials = BTreeMap::new();
    let mut last_usage = None;

    let e1 = process_sse_chunk(&chunk_with_content("Hi"), &mut partials, &mut last_usage).unwrap();
    assert_eq!(e1.len(), 1);

    let e2 = process_sse_chunk(
        &chunk_with_content(" there"),
        &mut partials,
        &mut last_usage,
    )
    .unwrap();
    assert_eq!(e2.len(), 1);

    let e3 = process_sse_chunk(&chunk_with_usage(10, 2), &mut partials, &mut last_usage).unwrap();
    assert!(e3.is_empty());

    assert!(last_usage.is_some());
}

#[test]
fn test_empty_chunk_is_noop() {
    let mut partials = BTreeMap::new();
    let mut last_usage = None;
    let empty = serde_json::json!({}).to_string();

    let events = process_sse_chunk(&empty, &mut partials, &mut last_usage).unwrap();
    assert!(events.is_empty());
}

/// Replicates the exact event-pipeline from `OpenAiCompatProvider::chat_stream`:
/// `process_sse_chunk` → for each event, feed `TextDelta`s through `ThinkParser`
/// and re-emit as `ReasoningDelta` with `echo_field = "thoughts"`. When the
/// SSE chunk also carries a dedicated reasoning field (`reasoning_content`
/// or `thoughts`), the inline `ThinkParser` is bypassed for that chunk so
/// the same thought text isn't published twice. This is the wiring that
/// lives in `chat_stream`; the unit test exercises it in isolation.
fn run_stream_pipeline(data: &str) -> Vec<ChatEvent> {
    let mut partials: BTreeMap<usize, PartialToolCall> = BTreeMap::new();
    let mut last_usage: Option<Value> = None;
    let mut think = ThinkParser::new();
    let mut out = Vec::new();
    // Mirrors the per-delta flag in `chat_stream`: when the chunk already
    // carries a dedicated reasoning field, the inline `ThinkParser` is not
    // the source of truth and must be skipped.
    let reasoning_via_field = delta_has_reasoning_field(data);

    for event in process_sse_chunk(data, &mut partials, &mut last_usage).unwrap() {
        match event {
            ChatEvent::TextDelta(content) => {
                eprintln!("pipeline: TextDelta({content:?})");
                let (text, thoughts) = if reasoning_via_field {
                    (content, String::new())
                } else {
                    think.feed(&content)
                };
                eprintln!("pipeline:   -> text={text:?}, thoughts={thoughts:?}");
                if !text.is_empty() {
                    out.push(ChatEvent::TextDelta(text));
                }
                if !thoughts.is_empty() {
                    out.push(ChatEvent::ReasoningDelta {
                        text: thoughts,
                        echo_field: Some("thoughts".to_string()),
                    });
                }
            }
            other => out.push(other),
        }
    }
    out
}

#[test]
fn think_parser_and_reasoning_field_do_not_double_publish() {
    // A delta that carries the SAME reasoning text BOTH as a dedicated field
    // (`reasoning_content`) and inline as `<think>…</think>` tags. The
    // pipeline must not publish the same thought text twice.
    let thought = "deep thoughts";
    let think_open = String::from_utf8(vec![b'<', b't', b'h', b'i', b'n', b'k']).unwrap() + ">";
    let think_close = String::from_utf8(vec![b'<', b'/']).unwrap() + "think>";
    let content = format!("{think_open}{thought}{think_close}answer");
    let data = serde_json::json!({
        "choices": [{
            "delta": {
                "content": content,
                "reasoning_content": thought
            }
        }]
    })
    .to_string();
    eprintln!("raw json: {data}");

    let events = run_stream_pipeline(&data);
    eprintln!("events: {events:#?}");
    let reasoning: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            ChatEvent::ReasoningDelta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        reasoning,
        vec!["deep thoughts"],
        "thought must appear exactly once, got: {reasoning:?}"
    );
}

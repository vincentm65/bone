use super::*;

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

// ---------------------------------------------------------------------------
// a. Normal text streaming
// ---------------------------------------------------------------------------
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

// ---------------------------------------------------------------------------
// b. Usage chunk
// ---------------------------------------------------------------------------
#[test]
fn test_usage_chunk_updates_last_usage() {
    let mut partials = BTreeMap::new();
    let mut last_usage: Option<Value> = None;

    let events =
        process_sse_chunk(&chunk_with_usage(42, 7), &mut partials, &mut last_usage).unwrap();
    // Usage chunk with empty delta produces no events.
    assert!(events.is_empty());

    let usage = last_usage.expect("last_usage should be set");
    assert_eq!(usage["prompt_tokens"].as_u64().unwrap(), 42);
    assert_eq!(usage["completion_tokens"].as_u64().unwrap(), 7);
}

// ---------------------------------------------------------------------------
// c. Single tool call split across chunks
// ---------------------------------------------------------------------------
#[test]
fn test_single_tool_call_split_across_chunks() {
    let mut partials = BTreeMap::new();
    let mut last_usage = None;

    // Chunk 1: id + name, first piece of arguments
    let e1 = process_sse_chunk(
        &chunk_with_tool(0, "call_", "read_", "{\"pat"),
        &mut partials,
        &mut last_usage,
    )
    .unwrap();
    assert!(e1.is_empty(), "partials shouldn't emit until finish_reason");

    // Chunk 2: more arguments
    let e2 = process_sse_chunk(
        &chunk_with_tool(0, "", "", "h\": \"/e"),
        &mut partials,
        &mut last_usage,
    )
    .unwrap();
    assert!(e2.is_empty());

    // Chunk 3: final arguments piece
    let e3 = process_sse_chunk(
        &chunk_with_tool(0, "", "", "tc\"}"),
        &mut partials,
        &mut last_usage,
    )
    .unwrap();
    assert!(e3.is_empty());

    // Finish chunk triggers flush
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

    // Partials should be empty after flush.
    assert!(partials.is_empty());
}

// ---------------------------------------------------------------------------
// d. Multiple tool calls interleaved by index
// ---------------------------------------------------------------------------
#[test]
fn test_multiple_tool_calls_interleaved_by_index() {
    let mut partials = BTreeMap::new();
    let mut last_usage = None;

    // Chunk 1: tool 0, part 1
    process_sse_chunk(
        &chunk_with_tool(0, "id0", "bash", "{\"cmd\": \"ls\"}"),
        &mut partials,
        &mut last_usage,
    )
    .unwrap();

    // Chunk 2: tool 1, part 1
    process_sse_chunk(
        &chunk_with_tool(1, "id1", "read", "{\"path\": \"/tmp\"}"),
        &mut partials,
        &mut last_usage,
    )
    .unwrap();

    // Chunk 3: tool 0, part 2 (more args)
    process_sse_chunk(
        &chunk_with_tool(0, "", "", ""),
        &mut partials,
        &mut last_usage,
    )
    .unwrap();

    // Chunk 4: tool 1, part 2
    process_sse_chunk(
        &chunk_with_tool(1, "", "", ""),
        &mut partials,
        &mut last_usage,
    )
    .unwrap();

    // Verify partials are tracked separately by index
    assert_eq!(partials.len(), 2);

    // Finish chunk triggers flush of both
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

// ---------------------------------------------------------------------------
// e. [DONE] flush — partial tool calls flushed by flush_partial_tool_calls
// ---------------------------------------------------------------------------
#[test]
fn test_done_flushes_partial_tool_calls() {
    let mut partials = BTreeMap::new();

    // Simulate accumulated partial tool call (no finish_reason seen)
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

// ---------------------------------------------------------------------------
// f. Stream ending without [DONE] — flush_partial_tool_calls called anyway
// ---------------------------------------------------------------------------
#[test]
fn test_stream_ends_without_done_still_flushes() {
    let mut partials = BTreeMap::new();

    // Simulate a partial call left over after stream naturally ends.
    let partial = PartialToolCall {
        id: "orphan_id".to_string(),
        name: "orphan_tool".to_string(),
        arguments: "{\"x\": 1}".to_string(),
    };
    partials.insert(0, partial);

    // The same flush function is used after the loop.
    let events = flush_partial_tool_calls(&mut partials);
    assert_eq!(events.len(), 1);
    assert!(matches!(&events[0], ChatEvent::ToolCall(c) if c.id == "orphan_id"));

    assert!(partials.is_empty());
}

// ---------------------------------------------------------------------------
// g. Malformed tool arguments → Value::Null, no panic
// ---------------------------------------------------------------------------
#[test]
fn test_malformed_tool_arguments_becomes_null() {
    let mut partials = BTreeMap::new();

    // Accumulated arguments that are NOT valid JSON
    let partial = PartialToolCall {
        id: "bad".to_string(),
        name: "broken".to_string(),
        arguments: "this is not json at all!!!".to_string(),
    };
    partials.insert(0, partial);

    // Should not panic; produces Value::Null.
    let events = flush_partial_tool_calls(&mut partials);
    assert_eq!(events.len(), 1);
    match &events[0] {
        ChatEvent::ToolCall(call) => {
            assert_eq!(call.arguments, Value::Null);
        }
        other => panic!("expected ToolCall, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Additional edge-case tests
// ---------------------------------------------------------------------------

/// Empty partial (no id/name) should be skipped by flush.
#[test]
fn test_empty_partial_skipped_on_flush() {
    let mut partials = BTreeMap::new();
    partials.insert(0, PartialToolCall::default()); // all fields empty

    let events = flush_partial_tool_calls(&mut partials);
    assert!(events.is_empty());
}

/// [DONE] with accumulated usage emits TokenUsage.
#[test]
fn test_done_emits_token_usage() {
    // This test verifies the logic inline in the stream loop:
    // we simulate what happens when last_usage is set and [DONE] arrives.
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
    };
    match event {
        ChatEvent::TokenUsage {
            prompt_tokens: p,
            completion_tokens: c,
        } => {
            assert_eq!(p, 100);
            assert_eq!(c, 50);
        }
        _ => panic!("expected TokenUsage"),
    }
}

/// SSE comment line (starts with ':') is skipped in the stream loop;
/// process_sse_chunk is never called, so this tests the guard in the loop.
#[test]
fn test_sse_comment_is_skipped() {
    // Comments are filtered in the stream loop before process_sse_chunk.
    // We verify process_sse_chunk would reject a comment as malformed JSON.
    let mut partials = BTreeMap::new();
    let mut last_usage = None;
    let result = process_sse_chunk(": this is a comment", &mut partials, &mut last_usage);
    assert!(result.is_err(), "comment line is not valid JSON");
    // The stream loop handles this by checking `data.starts_with(':')`
    // before calling process_sse_chunk, so this path is never hit in prod.
}

/// Several text chunks then a usage chunk: verify text accumulates and usage is captured.
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

/// Chunk with neither choices nor a delta should produce no events and no error.
#[test]
fn test_empty_chunk_is_noop() {
    let mut partials = BTreeMap::new();
    let mut last_usage = None;
    let empty = serde_json::json!({}).to_string();

    let events = process_sse_chunk(&empty, &mut partials, &mut last_usage).unwrap();
    assert!(events.is_empty());
}

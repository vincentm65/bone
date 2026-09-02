use super::*;
use crate::llm::providers::openai_compat::{
    ChatRequest, OaiContent, OaiImageUrl, OaiPart, OpenAiFunction, OpenAiMessage, OpenAiTool,
    OpenAiToolCall, OpenAiToolCallFunction, StreamOptions,
};

fn debug() -> CacheDebug {
    CacheDebug(Arc::new(Inner {
        file: Mutex::new(tempfile::tempfile().expect("temporary diagnostic file")),
        state: Mutex::new(State::default()),
        salt: [7; 32],
    }))
}

fn message(role: &str, content: OaiContent) -> OpenAiMessage {
    OpenAiMessage {
        role: role.to_string(),
        content: Some(content),
        tool_calls: Vec::new(),
        tool_call_id: None,
        name: None,
        reasoning: BTreeMap::new(),
    }
}

fn request(messages: Vec<OpenAiMessage>, tools: Vec<OpenAiTool>) -> ChatRequest {
    ChatRequest {
        model: "test-model".to_string(),
        messages,
        stream: true,
        tools,
        stream_options: Some(StreamOptions {
            include_usage: true,
        }),
        max_tokens: None,
        reasoning_effort: None,
        prompt_cache_key: Some("conversation-1".to_string()),
    }
}

fn tool(description: &str, parameters: Value) -> OpenAiTool {
    OpenAiTool {
        r#type: "function",
        function: OpenAiFunction {
            name: "safe_tool".to_string(),
            description: description.to_string(),
            parameters,
        },
    }
}

#[test]
fn classifies_ordered_message_changes() {
    let old = ["a", "b"];
    assert_eq!(classify(&old, &old), "identical");
    assert_eq!(classify(&old, &["a", "b", "c"]), "append");
    assert_eq!(classify(&old, &["a"]), "truncate");
    assert_eq!(classify(&old, &["a", "x"]), "mutate");
    assert_eq!(classify(&old, &["x"]), "reset");
}

#[test]
fn diff_identifies_system_tools_and_changed_fields() {
    let debug = debug();
    let old = request(
        vec![
            message("system", OaiContent::Text("stable system".to_string())),
            message("user", OaiContent::Text("old user".to_string())),
        ],
        vec![tool("old description", json!({"type": "object"}))],
    );
    let new = request(
        vec![
            message("system", OaiContent::Text("changed system".to_string())),
            message("user", OaiContent::Text("new user".to_string())),
        ],
        vec![tool("new description", json!({"type": "object"}))],
    );
    let mut old_snapshot = snapshot(&debug, &old);
    old_snapshot.timestamp_ms = 100;
    let mut new_snapshot = snapshot(&debug, &new);
    new_snapshot.timestamp_ms = 175;

    let change = diff(&old_snapshot, &new_snapshot);
    assert_eq!(change["classification"], "reset");
    assert_eq!(change["first_changed_message"], 0);
    assert_eq!(change["system_message_changed"], true);
    assert_eq!(change["tool_definition_changed"], true);
    assert_eq!(change["gap_ms"], 75);
    assert_eq!(change["changed_fields"][0]["index"], 0);
    assert!(
        change["changed_fields"][0]["fields"]
            .as_array()
            .is_some_and(|fields| fields.iter().any(|field| field == "content.text"))
    );
}

#[test]
fn aggregation_is_token_weighted_and_excludes_unknown_cache_usage() {
    let mut aggregate = Aggregate::default();
    account(
        &mut aggregate,
        "completed",
        Some(Usage {
            prompt: 100,
            completion: 10,
            cached: Some(0),
        }),
    );
    account(
        &mut aggregate,
        "completed",
        Some(Usage {
            prompt: 10_000,
            completion: 20,
            cached: Some(9_900),
        }),
    );
    account(
        &mut aggregate,
        "completed",
        Some(Usage {
            prompt: 500,
            completion: 5,
            cached: None,
        }),
    );
    account(&mut aggregate, "completed", None);
    account(&mut aggregate, "stream_error", None);
    account(&mut aggregate, "abandoned_or_cancelled", None);

    assert_eq!(aggregate.prompt, 10_100);
    assert_eq!(aggregate.cached, 9_900);
    assert_eq!(aggregate.reported_with_cache_count, 2);
    assert_eq!(aggregate.reported_without_cache_count, 1);
    assert_eq!(aggregate.missing_usage_count, 1);
    assert_eq!(aggregate.failures, 1);
    assert_eq!(aggregate.abandoned, 1);
    assert!((cache_rate(aggregate.prompt, aggregate.cached) - 0.980_198).abs() < 0.000_001);
}

#[test]
fn usage_metrics_preserve_unknown_and_flag_provider_anomalies() {
    assert_eq!(
        usage_metrics(Usage {
            prompt: 10,
            completion: 1,
            cached: None,
        }),
        UsageMetrics {
            uncached: None,
            rate: None,
            cached_greater_than_prompt: false,
        }
    );
    assert_eq!(
        usage_metrics(Usage {
            prompt: 10,
            completion: 1,
            cached: Some(14),
        }),
        UsageMetrics {
            uncached: Some(0),
            rate: Some(1.4),
            cached_greater_than_prompt: true,
        }
    );
}

#[test]
fn safe_diagnostics_do_not_contain_request_plaintext() {
    let debug = debug();
    let mut assistant = message(
        "assistant",
        OaiContent::Text("SECRET_CONTENT_7F29".to_string()),
    );
    assistant.tool_calls.push(OpenAiToolCall {
        id: "call-safe".to_string(),
        r#type: "function",
        function: OpenAiToolCallFunction {
            name: "safe_tool".to_string(),
            arguments: "SECRET_ARGUMENTS_8B31".to_string(),
        },
    });
    assistant.reasoning.insert(
        "reasoning_content".to_string(),
        "SECRET_REASONING_4E12".to_string(),
    );
    let image = message(
        "user",
        OaiContent::Parts(vec![OaiPart::ImageUrl {
            image_url: OaiImageUrl {
                url: "data:image/png;base64,SECRET_IMAGE_9A77".to_string(),
            },
        }]),
    );
    let request = request(
        vec![assistant, image],
        vec![tool(
            "SECRET_DESCRIPTION_3C44",
            json!({"description": "SECRET_SCHEMA_2D55"}),
        )],
    );

    let safe = json!({
        "messages": message_events(&debug, &request),
        "tools": tool_events(&debug, &request),
    })
    .to_string();
    for secret in [
        "SECRET_CONTENT_7F29",
        "SECRET_ARGUMENTS_8B31",
        "SECRET_REASONING_4E12",
        "SECRET_IMAGE_9A77",
        "SECRET_DESCRIPTION_3C44",
        "SECRET_SCHEMA_2D55",
    ] {
        assert!(!safe.contains(secret), "diagnostic leaked {secret}");
    }
    assert!(safe.contains("safe_tool"));
    assert!(safe.contains("image/png"));
    assert!(safe.contains("fingerprint"));
}

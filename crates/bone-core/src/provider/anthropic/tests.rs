use super::*;

fn config() -> ProviderConfig {
    serde_json::from_value(json!({ "api": "anthropic", "model": "claude-test" })).unwrap()
}

fn user(text: &str) -> ChatMessage {
    ChatMessage::User {
        content: text.into(),
        images: Vec::new(),
    }
}

fn tool(id: &str, content: &str, is_error: bool) -> ChatMessage {
    ChatMessage::Tool {
        call_id: id.into(),
        content: content.into(),
        images: Vec::new(),
        is_error,
    }
}

fn call(id: &str, arguments: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: "shell".into(),
        arguments: arguments.into(),
    }
}

fn shell() -> Vec<crate::tools::ToolSpec> {
    vec![crate::tools::ToolSpec {
        name: "shell".into(),
        description: "run".into(),
        parameters: json!({ "type": "object" }),
    }]
}

fn body(config: &ProviderConfig, messages: &[ChatMessage]) -> Value {
    let tools = shell();
    let req = CompletionRequest {
        session_id: "test",
        messages,
        tools: &tools,
        depth: 0,
    };
    request_body(config, &req).0
}

fn run(events: &[Value]) -> (Result<Completion, ProviderError>, Vec<Delta>) {
    let mut acc = Accumulator::default();
    let mut deltas = Vec::new();
    for e in events {
        if acc.apply(&e.to_string(), &mut |d| deltas.push(d)).unwrap() {
            break;
        }
    }
    (acc.finish("claude-test", "fp".into()), deltas)
}

#[test]
fn converts_messages_and_places_cache_checkpoints() {
    let messages = vec![
        ChatMessage::System {
            content: "system".into(),
        },
        user("hello"),
        ChatMessage::Assistant {
            content: "checking".into(),
            reasoning: "not replayed".into(),
            tool_calls: vec![call("c1", r#"{"command":"pwd"}"#)],
            reasoning_state: None,
        },
        tool("c1", "ok", false),
        tool("c2", "failed", true),
    ];
    let mut config = config();
    let b = body(&config, &messages);
    assert_eq!(b["max_tokens"], DEFAULT_MAX_TOKENS);
    assert_eq!(
        b["thinking"],
        json!({ "type": "adaptive", "display": "summarized" })
    );
    assert_eq!(b["messages"].as_array().unwrap().len(), 3);
    assert_eq!(
        b["messages"][1]["content"][1]["input"],
        json!({ "command": "pwd" })
    );
    assert!(b["messages"][2]["content"][0].get("is_error").is_none());
    assert_eq!(b["messages"][2]["content"][1]["is_error"], true);
    let ephemeral = json!({ "type": "ephemeral" });
    assert_eq!(b["tools"][0]["cache_control"], ephemeral);
    assert_eq!(b["system"][0]["cache_control"], ephemeral);
    assert_eq!(b["messages"][0]["content"][0]["cache_control"], ephemeral);
    assert_eq!(b["messages"][2]["content"][1]["cache_control"], ephemeral);
    assert!(
        b["messages"][2]["content"][0]
            .get("cache_control")
            .is_none()
    );

    // The entry's `body` adjusts any field; `null` removes one.
    config.options = json!({ "cache": false });
    config.body = json!({ "max_tokens": 1024, "thinking": null });
    config.reasoning_effort = Some("high".into());
    let (b, _) = configured(body(&config, &messages), Vec::new(), &config);
    assert_eq!(b["max_tokens"], 1024);
    assert!(!b.to_string().contains("cache_control"));
    assert!(b.get("thinking").is_none());
    assert_eq!(b["output_config"], json!({ "effort": "high" }));
}

#[test]
fn images_and_empty_text() {
    let image = ImageAttachment {
        id: "i".into(),
        name: "a.png".into(),
        mime_type: "image/png".into(),
        width: 1,
        height: 1,
        bytes: 3,
        data: Some("AAAA".into()),
    };
    let messages = vec![
        ChatMessage::User {
            content: String::new(),
            images: vec![image.clone()],
        },
        ChatMessage::Assistant {
            content: "\n\n".into(),
            reasoning: String::new(),
            tool_calls: vec![call("c1", "not json")],
            reasoning_state: None,
        },
        ChatMessage::Tool {
            call_id: "c1".into(),
            content: "shot".into(),
            images: vec![image],
            is_error: false,
        },
    ];
    let b = body(&config(), &messages);
    // No blank text blocks: Anthropic rejects them.
    assert_eq!(b["messages"][0]["content"].as_array().unwrap().len(), 1);
    assert_eq!(
        b["messages"][0]["content"][0]["source"]["media_type"],
        "image/png"
    );
    assert_eq!(
        b["messages"][1]["content"][0]["input"],
        json!({ "invalid_arguments": "not json" })
    );
    let result = &b["messages"][2]["content"][0]["content"];
    assert_eq!(result[0], json!({ "type": "text", "text": "shot" }));
    assert_eq!(result[1]["type"], "image");
}

#[test]
fn streams_text_tools_and_cache_usage() {
    let (c, deltas) = run(&[
        json!({"type":"message_start","message":{"usage":{"input_tokens":3,"cache_creation_input_tokens":10,"cache_read_input_tokens":20,"output_tokens":1}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}),
        json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"c1","name":"shell","input":{}}}),
        json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"command\":"}}),
        json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"\"pwd\"}"}}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":7}}),
        json!({"type":"message_delta","usage":{"input_tokens":8,"cache_creation_input_tokens":0,"cache_read_input_tokens":30,"output_tokens":5}}),
        json!({"type":"message_delta","usage":{"output_tokens":7}}),
        json!({"type":"message_stop"}),
    ]);
    let c = c.unwrap();
    assert_eq!(c.content, "Hi");
    assert_eq!(deltas, vec![Delta::Text("Hi".into())]);
    assert_eq!(c.tool_calls[0].arguments, r#"{"command":"pwd"}"#);
    assert_eq!(
        c.reasoning_state, None,
        "nothing to replay without thinking"
    );
    let u = c.usage.unwrap();
    assert_eq!(
        (u.input_tokens, u.output_tokens, u.cached_tokens),
        (38, 7, Some(30))
    );
}

#[test]
fn stop_reasons_and_errors() {
    let start = json!({"type":"message_start","message":{"usage":{}}});
    let stop = json!({"type":"message_stop"});
    for (reason, says) in [
        ("max_tokens", "incomplete"),
        ("model_context_window_exceeded", "context window"),
        ("refusal", "declined"),
    ] {
        let delta = json!({"type":"message_delta","delta":{"stop_reason":reason}});
        let (c, _) = run(&[start.clone(), delta, stop.clone()]);
        assert!(c.unwrap_err().0.contains(says));
    }
    let mut acc = Accumulator::default();
    let err = acc
        .apply(
            r#"{"type":"error","error":{"type":"overloaded_error"}}"#,
            &mut |_| {},
        )
        .unwrap_err();
    assert!(err.0.contains("overloaded"), "{err}");
    assert!(acc.apply("not json", &mut |_| {}).is_err());
    assert!(
        acc.apply(r#"{"type":"message_stop"}"#, &mut |_| {})
            .is_err()
    );
}

#[test]
fn thinking_goes_back_only_while_its_history_is_unchanged() {
    let (c, deltas) = run(&[
        json!({"type":"message_start","message":{"usage":{}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"plan"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig"}}),
        json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"c1","name":"shell","input":{"command":"pwd"}}}),
        json!({"type":"message_stop"}),
    ]);
    let c = c.unwrap();
    assert_eq!(deltas, vec![Delta::Reasoning("plan".into())]);
    let thinking = json!({ "type": "thinking", "thinking": "plan", "signature": "sig" });

    // The fingerprint is of the request that produced the reply.
    let asked = vec![user("go")];
    let tools = shell();
    let req = CompletionRequest {
        session_id: "t",
        messages: &asked,
        tools: &tools,
        depth: 0,
    };
    let (_, fingerprint) = request_body(&config(), &req);
    let mut state = c.reasoning_state.unwrap();
    assert_eq!(state.items[0], thinking);
    state.prefix = Some(fingerprint);
    let history = |first: &str, state: &ReasoningState| {
        vec![
            user(first),
            ChatMessage::Assistant {
                content: String::new(),
                reasoning: c.reasoning.clone(),
                tool_calls: c.tool_calls.clone(),
                reasoning_state: Some(state.clone()),
            },
            tool("c1", "/", false),
        ]
    };
    let b = body(&config(), &history("go", &state));
    assert_eq!(b["messages"][1]["content"][0], thinking);
    assert_eq!(b["messages"][1]["content"][1]["id"], "c1");
    assert!(b.get("thinking").is_some());
    // An edited history, or another model: the thinking stays out, and the
    // tool loop it belonged to finishes without thinking.
    let b = body(&config(), &history("went", &state));
    assert_eq!(b["messages"][1]["content"][0]["type"], "tool_use");
    assert!(b.get("thinking").is_none());
    let mut other = config();
    other.model = "claude-other".into();
    let b = body(&other, &history("go", &state));
    assert_eq!(b["messages"][1]["content"][0]["type"], "tool_use");
}

#[tokio::test]
async fn http_headers_path_stream_and_premature_eof() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    for complete in [true, false] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut config = config();
        config.base_url = format!("http://{}/v1/", listener.local_addr().unwrap());
        config.api_key = Some("secret".into());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let header_end = loop {
                let mut chunk = [0; 4096];
                let n = socket.read(&mut chunk).await.unwrap();
                assert!(n > 0);
                buf.extend_from_slice(&chunk[..n]);
                if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break i + 4;
                }
            };
            let headers = String::from_utf8_lossy(&buf[..header_end]).to_lowercase();
            assert!(headers.starts_with("post /v1/messages http/1.1"));
            assert!(headers.contains("x-api-key: secret"));
            assert!(headers.contains("anthropic-version: 2023-06-01"));
            assert!(!headers.contains("authorization"));
            let len: usize = headers
                .lines()
                .find_map(|l| l.strip_prefix("content-length: "))
                .unwrap()
                .parse()
                .unwrap();
            while buf.len() < header_end + len {
                let mut chunk = [0; 4096];
                let n = socket.read(&mut chunk).await.unwrap();
                assert!(n > 0);
                buf.extend_from_slice(&chunk[..n]);
            }
            let body: Value = serde_json::from_slice(&buf[header_end..header_end + len]).unwrap();
            assert_eq!(body["model"], "claude-test");
            let mut stream = concat!(
                "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":4}}}\n\n",
                "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
                "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hello\"}}\n\n"
            )
            .to_string();
            if complete {
                stream.push_str("data: {\"type\":\"message_stop\"}\n\n");
            }
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", stream.len()).as_bytes()).await.unwrap();
            for chunk in stream.as_bytes().chunks(7) {
                socket.write_all(chunk).await.unwrap();
            }
        });
        let provider = crate::provider::built_in(config);
        let messages = [user("hello")];
        let result = provider
            .complete(
                CompletionRequest {
                    session_id: "test",
                    messages: &messages,
                    tools: &[],
                    depth: 0,
                },
                &mut |_| {},
            )
            .await;
        if complete {
            assert_eq!(result.unwrap().content, "hello");
        } else {
            assert!(
                result
                    .unwrap_err()
                    .0
                    .contains("before the response completed")
            );
        }
        server.await.unwrap();
    }
}

#[test]
fn retains_previous_cache_boundary_beyond_lookback() {
    let mut messages = vec![
        ChatMessage::System {
            content: "system".into(),
        },
        user("previous request"),
        ChatMessage::Assistant {
            content: "reply".into(),
            reasoning: String::new(),
            tool_calls: vec![],
            reasoning_state: None,
        },
    ];
    messages.extend((0..25).map(|i| user(&format!("new block {i}"))));
    let b = body(&config(), &messages);
    assert_eq!(
        b["messages"][0]["content"][0]["cache_control"]["type"],
        "ephemeral"
    );
    assert_eq!(
        b["messages"][2]["content"][24]["cache_control"]["type"],
        "ephemeral"
    );
    assert_eq!(b.to_string().matches("cache_control").count(), 4);
}

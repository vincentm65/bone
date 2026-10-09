//! Full stack: client -> server -> core -> OpenAI-compatible HTTP provider,
//! against a fake SSE server.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bone_client::{Client, EventStream};
use bone_core::Core;
use bone_core::config::{CoreConfig, ProviderConfig};
use bone_proto::methods::*;
use bone_proto::types::{ChatMessage, DeltaKind, TurnOutcome};
use bone_server::Server;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Serve one canned SSE body per request, recording each request body.
async fn fake_openai(bodies: Vec<String>) -> (String, Arc<Mutex<Vec<Value>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    tokio::spawn(async move {
        for body in bodies {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            let (head_end, len) = loop {
                let n = sock.read(&mut chunk).await.unwrap();
                buf.extend_from_slice(&chunk[..n]);
                if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&buf[..i]).to_lowercase();
                    assert!(head.starts_with("post /v1/chat/completions"), "{head}");
                    assert!(head.contains("authorization: bearer sk-test"), "{head}");
                    let len = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .map(|v| v.trim().parse::<usize>().unwrap())
                        .unwrap();
                    break (i + 4, len);
                }
            };
            while buf.len() < head_end + len {
                let n = sock.read(&mut chunk).await.unwrap();
                buf.extend_from_slice(&chunk[..n]);
            }
            seen2
                .lock()
                .unwrap()
                .push(serde_json::from_slice(&buf[head_end..]).unwrap());
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{body}"
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
            sock.shutdown().await.unwrap();
        }
    });
    (url, seen)
}

fn sse(chunks: &[Value]) -> String {
    let mut out: String = chunks.iter().map(|c| format!("data: {c}\n\n")).collect();
    out.push_str("data: [DONE]\n\n");
    out
}

async fn next_event(events: &mut EventStream) -> bone_client::Event {
    tokio::time::timeout(Duration::from_secs(10), events.recv())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn turn_with_tool_call_over_http() {
    let (url, seen) = fake_openai(vec![
        sse(&[
            json!({"choices":[{"delta":{"reasoning_content":"let me run it"}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"shell","arguments":""}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"command\":\"echo hi\"}"}}]}}]}),
            json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
        ]),
        sse(&[
            json!({"choices":[{"delta":{"content":"It printed "}}]}),
            json!({"choices":[{"delta":{"content":"hi."}}]}),
            json!({"choices":[],"usage":{"prompt_tokens":42,"completion_tokens":3}}),
        ]),
    ])
    .await;

    let data = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let core = Core::new(CoreConfig {
        provider: ProviderConfig {
            supports_images: None,
            kind: None,
            options: serde_json::Value::Null,
            base_url: url,
            model: "test-model".into(),
            api_key: Some("sk-test".into()),
            reasoning_effort: None,
            stream_usage: true,
            replay_reasoning: false,
        },
        system_prompt: Some("SYSTEM".into()),
        data_dir: data.path().to_owned(),
        parallel_tools: true,
        compact: Default::default(),
    });
    let server = Server::new(Arc::new(core));
    let (client, mut events) = Client::new(server.connect_in_process());
    client.initialize("test").await.unwrap();

    let info = client
        .request::<SessionCreate>(SessionCreateParams {
            cwd: Some(work.path().to_string_lossy().into()),
        })
        .await
        .unwrap();
    let turn = client
        .request::<TurnStart>(TurnStartParams {
            session_id: info.session_id.clone(),
            text: "run echo hi".into(),
            images: Vec::new(),
        })
        .await
        .unwrap();

    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tool_output = None;
    let mut usage = None;
    let outcome = loop {
        let e = next_event(&mut events).await;
        if let Some(d) = e.parse::<MessageDelta>() {
            let d = d.unwrap();
            assert_eq!(d.turn_id, turn.turn_id);
            match d.kind {
                DeltaKind::Text => text.push_str(&d.text),
                DeltaKind::Reasoning => reasoning.push_str(&d.text),
            }
        } else if let Some(f) = e.parse::<ToolFinished>() {
            tool_output = Some(f.unwrap().output);
        } else if let Some(m) = e.parse::<MessageCompleted>() {
            usage = m.unwrap().usage.or(usage);
        } else if let Some(f) = e.parse::<TurnFinished>() {
            break f.unwrap().outcome;
        }
    };
    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(text, "It printed hi.");
    assert_eq!(reasoning, "let me run it");
    assert_eq!(tool_output.as_deref(), Some("hi\n[exit code: 0]"));
    assert_eq!(
        usage.map(|u| (u.input_tokens, u.output_tokens)),
        Some((42, 3))
    );

    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen[0]["model"], "test-model");
    assert_eq!(seen[0]["stream"], true);
    assert_eq!(seen[0]["messages"][0]["role"], "system");
    assert!(
        seen[0]["messages"][0]["content"]
            .as_str()
            .unwrap()
            .starts_with("SYSTEM\n\nWorking directory: ")
    );
    let tools: Vec<&str> = seen[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(tools, ["read_file", "write_file", "edit_file", "shell"]);
    assert_eq!(
        seen[1]["messages"][3],
        json!({"role":"tool","tool_call_id":"call_a","content":"hi\n[exit code: 0]"})
    );

    let msgs = client
        .request::<SessionMessages>(SessionRef {
            session_id: info.session_id,
        })
        .await
        .unwrap();
    assert_eq!(msgs.messages.len(), 4);
    assert!(
        matches!(&msgs.messages[3], ChatMessage::Assistant { content, .. } if content == "It printed hi.")
    );
}

#[tokio::test]
async fn lua_tool_and_hooks_over_http() {
    let (url, seen) = fake_openai(vec![
        sse(&[json!({"choices":[{"delta":{"tool_calls":[
            {"index":0,"id":"c1","function":{"name":"word_count","arguments":"{\"text\":\"a b c\"}"}},
            {"index":1,"id":"c2","function":{"name":"shell","arguments":"{\"command\":\"rm -rf /\"}"}}
        ]}}]})]),
        sse(&[json!({"choices":[{"delta":{"content":"ok"}}]})]),
    ])
    .await;

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("core.lua"),
        format!(
            r#"
            bone.config.providers.fake = {{ base_url = "{url}", model = "m", api_key = "sk-test" }}
            bone.config.provider = "fake"
            bone.config.system_prompt = function(ctx) return "LUA PROMPT" end
            bone.tool.register {{
              name = "word_count",
              description = "Count words",
              parameters = {{ type = "object", properties = {{ text = {{ type = "string" }} }} }},
              run = function(args) local n = 0; for _ in args.text:gmatch("%S+") do n = n + 1 end; return tostring(n) end,
            }}
            bone.hook("tool_call", function(ev)
              local answer = bone.ask({{ kind = "approval", tool = ev.name }})
              if answer ~= "allow" then return {{ deny = "not approved" }} end
              if ev.name == "shell" then return {{ deny = "shell is off" }} end
            end)
            "#
        ),
    )
    .unwrap();
    let work = tempfile::tempdir().unwrap();
    let loaded = bone_core::scripting::load(dir.path()).unwrap();
    assert_eq!(loaded.config.data_dir, dir.path());
    let server = Server::new(Arc::new(Core::from_loaded(loaded)));
    let (client, mut events) = Client::new(server.connect_in_process());
    client.initialize("test").await.unwrap();
    let info = client
        .request::<SessionCreate>(SessionCreateParams {
            cwd: Some(work.path().to_string_lossy().into()),
        })
        .await
        .unwrap();
    client
        .request::<TurnStart>(TurnStartParams {
            session_id: info.session_id,
            text: "count".into(),
            images: Vec::new(),
        })
        .await
        .unwrap();
    // The inline hook asks (over the protocol) before both calls;
    // the client allows them, then the user's hook still refuses shell.
    let mut asked = Vec::new();
    loop {
        let e = next_event(&mut events).await;
        if let Some(Ok(q)) = e.parse::<AskRequested>() {
            asked.push(q.question["tool"].as_str().unwrap_or_default().to_owned());
            client
                .request::<AskRespond>(AskRespondParams {
                    ask_id: q.ask_id,
                    answer: json!("allow"),
                })
                .await
                .unwrap();
        }
        if let Some(f) = e.parse::<TurnFinished>() {
            assert_eq!(f.unwrap().outcome, TurnOutcome::Completed);
            break;
        }
    }
    assert_eq!(asked, ["word_count", "shell"]);
    let seen = seen.lock().unwrap().clone();
    assert_eq!(
        seen[0]["messages"][0]["content"]
            .as_str()
            .unwrap()
            .lines()
            .next(),
        Some("LUA PROMPT")
    );
    let tools: Vec<&str> = seen[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap())
        .collect();
    assert!(tools.contains(&"word_count"), "{tools:?}");
    assert_eq!(
        seen[1]["messages"][3],
        json!({"role":"tool","tool_call_id":"c1","content":"3"})
    );
    assert_eq!(
        seen[1]["messages"][4],
        json!({"role":"tool","tool_call_id":"c2","content":"shell is off"})
    );
}

//! OpenAI-compatible `/chat/completions` streaming provider. Covers OpenAI,
//! DeepSeek, OpenRouter, MiniMax, tabbyAPI, vLLM, llama.cpp and friends.

use std::collections::BTreeMap;
use std::time::Duration;

use bone_proto::types::{ChatMessage, ToolCall, Usage};
use futures_util::StreamExt;
use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};

use super::sse::SseParser;
use super::{Completion, CompletionRequest, Delta, DeltaSink, Provider, ProviderError};
use crate::config::ProviderConfig;

const MAX_ATTEMPTS: u32 = 3;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

pub struct OpenAiProvider {
    http: reqwest::Client,
    config: ProviderConfig,
}

impl OpenAiProvider {
    pub fn new(config: ProviderConfig) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .expect("reqwest client builds");
        OpenAiProvider { http, config }
    }

    fn url(&self) -> String {
        format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        )
    }

    /// Send the request, retrying connection failures, 429 and 5xx. Nothing has
    /// streamed yet at this point, so retrying is invisible to the caller.
    async fn send(&self, body: &Value) -> Result<reqwest::Response, ProviderError> {
        let mut attempt = 1;
        loop {
            let mut req = self.http.post(self.url()).json(body);
            if let Some(key) = self.config.api_key.as_deref().filter(|k| !k.is_empty()) {
                req = req.bearer_auth(key);
            }
            let retryable = match req.send().await {
                Ok(resp) if resp.status().is_success() => return Ok(resp),
                Ok(resp) => {
                    let status = resp.status();
                    let text = resp.text().await.unwrap_or_default();
                    let err = ProviderError(format!("HTTP {status}: {}", text.trim()));
                    if !(status.as_u16() == 429 || status.is_server_error()) {
                        return Err(err);
                    }
                    err
                }
                Err(e) if e.is_connect() || e.is_timeout() => {
                    ProviderError(format!("request failed: {e}"))
                }
                Err(e) => return Err(ProviderError(format!("request failed: {e}"))),
            };
            if attempt >= MAX_ATTEMPTS {
                return Err(retryable);
            }
            tokio::time::sleep(Duration::from_secs(1 << (attempt - 1))).await;
            attempt += 1;
        }
    }

    async fn stream(
        &self,
        req: CompletionRequest<'_>,
        on_delta: DeltaSink<'_>,
    ) -> Result<Completion, ProviderError> {
        let body = request_body(&self.config, &req);
        let resp = self.send(&body).await?;
        let mut bytes = resp.bytes_stream();
        let mut parser = SseParser::default();
        let mut acc = Accumulator::default();
        while let Some(chunk) = bytes.next().await {
            let chunk = chunk.map_err(|e| ProviderError(format!("stream error: {e}")))?;
            for data in parser.push(&chunk) {
                if acc.apply(&data, on_delta)? {
                    return Ok(acc.finish());
                }
            }
        }
        if let Some(data) = parser.finish() {
            acc.apply(&data, on_delta)?;
        }
        Ok(acc.finish())
    }
}

impl Provider for OpenAiProvider {
    fn complete<'a>(
        &'a self,
        req: CompletionRequest<'a>,
        on_delta: DeltaSink<'a>,
    ) -> BoxFuture<'a, Result<Completion, ProviderError>> {
        Box::pin(self.stream(req, on_delta))
    }
}

fn request_body(config: &ProviderConfig, req: &CompletionRequest<'_>) -> Value {
    let mut body = json!({
        "model": config.model,
        "messages": req.messages.iter().map(wire_message).collect::<Vec<_>>(),
        "stream": true,
    });
    if config.stream_usage {
        body["stream_options"] = json!({ "include_usage": true });
    }
    if !req.tools.is_empty() {
        body["tools"] = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    }
                })
            })
            .collect();
    }
    if let Some(effort) = config.reasoning_effort.as_deref().filter(|e| !e.is_empty()) {
        body["reasoning_effort"] = json!(effort);
    }
    body
}

fn wire_message(msg: &ChatMessage) -> Value {
    match msg {
        ChatMessage::System { content } => json!({ "role": "system", "content": content }),
        ChatMessage::User { content } => json!({ "role": "user", "content": content }),
        ChatMessage::Assistant {
            content,
            tool_calls,
            ..
        } => {
            let mut m = json!({
                "role": "assistant",
                "content": if content.is_empty() { Value::Null } else { json!(content) },
            });
            if !tool_calls.is_empty() {
                m["tool_calls"] = tool_calls
                    .iter()
                    .map(|c| {
                        json!({
                            "id": c.id,
                            "type": "function",
                            "function": { "name": c.name, "arguments": c.arguments },
                        })
                    })
                    .collect();
            }
            m
        }
        ChatMessage::Tool {
            call_id, content, ..
        } => {
            json!({ "role": "tool", "tool_call_id": call_id, "content": content })
        }
    }
}

// ---- stream chunks -------------------------------------------------------
// Every field is optional and nullable: compatible servers differ a lot.

#[derive(Deserialize)]
struct Chunk {
    #[serde(default)]
    choices: Option<Vec<Choice>>,
    #[serde(default)]
    usage: Option<ChunkUsage>,
    #[serde(default)]
    error: Option<Value>,
}

#[derive(Deserialize)]
struct Choice {
    #[serde(default)]
    delta: Option<ChoiceDelta>,
}

#[derive(Deserialize)]
struct ChoiceDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ToolCallDelta>>,
}

#[derive(Deserialize)]
struct ToolCallDelta {
    #[serde(default)]
    index: Option<usize>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<FunctionDelta>,
}

#[derive(Deserialize)]
struct FunctionDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct ChunkUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
}

#[derive(Default)]
struct Accumulator {
    content: String,
    reasoning: String,
    calls: BTreeMap<usize, ToolCall>,
    usage: Option<Usage>,
}

impl Accumulator {
    /// Apply one SSE data payload. Returns true at `[DONE]`.
    fn apply(&mut self, data: &str, on_delta: DeltaSink<'_>) -> Result<bool, ProviderError> {
        if data.trim() == "[DONE]" {
            return Ok(true);
        }
        let chunk: Chunk = serde_json::from_str(data)
            .map_err(|e| ProviderError(format!("bad stream chunk ({e}): {data}")))?;
        if let Some(err) = chunk.error {
            return Err(ProviderError(format!("provider error: {err}")));
        }
        if let Some(u) = chunk.usage {
            self.usage = Some(Usage {
                input_tokens: u.prompt_tokens,
                output_tokens: u.completion_tokens,
            });
        }
        let deltas = chunk
            .choices
            .unwrap_or_default()
            .into_iter()
            .filter_map(|c| c.delta);
        for d in deltas {
            if let Some(text) = d
                .reasoning_content
                .or(d.reasoning)
                .filter(|t| !t.is_empty())
            {
                self.reasoning.push_str(&text);
                on_delta(Delta::Reasoning(text));
            }
            if let Some(text) = d.content.filter(|t| !t.is_empty()) {
                self.content.push_str(&text);
                on_delta(Delta::Text(text));
            }
            for (pos, tc) in d.tool_calls.unwrap_or_default().into_iter().enumerate() {
                let index = tc.index.unwrap_or(pos);
                let call = self.calls.entry(index).or_insert_with(|| ToolCall {
                    id: String::new(),
                    name: String::new(),
                    arguments: String::new(),
                });
                if let Some(id) = tc.id.filter(|id| !id.is_empty()) {
                    call.id = id;
                }
                if let Some(f) = tc.function {
                    if let Some(name) = f.name.filter(|n| !n.is_empty())
                        && call.name.is_empty()
                    {
                        call.name = name;
                    }
                    if let Some(args) = f.arguments {
                        call.arguments.push_str(&args);
                    }
                }
            }
        }
        Ok(false)
    }

    fn finish(self) -> Completion {
        let tool_calls = self
            .calls
            .into_iter()
            .map(|(index, mut call)| {
                if call.id.is_empty() {
                    call.id = format!("call_{index}");
                }
                call
            })
            .collect();
        Completion {
            content: self.content,
            reasoning: self.reasoning,
            tool_calls,
            usage: self.usage,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(lines: &[&str]) -> (Completion, Vec<Delta>) {
        let mut acc = Accumulator::default();
        let mut deltas = Vec::new();
        let mut sink = |d: Delta| deltas.push(d);
        for line in lines {
            if acc.apply(line, &mut sink).unwrap() {
                break;
            }
        }
        (acc.finish(), deltas)
    }

    #[test]
    fn accumulates_text_reasoning_and_tool_calls() {
        let (c, deltas) = run(&[
            r#"{"choices":[{"delta":{"role":"assistant","reasoning_content":"think"}}]}"#,
            r#"{"choices":[{"delta":{"content":"Hi"}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"read_file","arguments":"{\"pa"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"a\"}"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"name":"shell","arguments":"{}"}}]}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}],"usage":null}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5}}"#,
            "[DONE]",
            r#"{"choices":[{"delta":{"content":"after done"}}]}"#,
        ]);
        assert_eq!(c.content, "Hi");
        assert_eq!(c.reasoning, "think");
        assert_eq!(
            c.tool_calls,
            vec![
                ToolCall {
                    id: "c1".into(),
                    name: "read_file".into(),
                    arguments: r#"{"path":"a"}"#.into()
                },
                ToolCall {
                    id: "call_1".into(),
                    name: "shell".into(),
                    arguments: "{}".into()
                },
            ]
        );
        assert_eq!(
            c.usage,
            Some(Usage {
                input_tokens: 10,
                output_tokens: 5
            })
        );
        assert_eq!(
            deltas,
            vec![Delta::Reasoning("think".into()), Delta::Text("Hi".into())]
        );
    }

    #[test]
    fn tolerates_nulls_and_reports_errors() {
        let (c, _) = run(&[r#"{"choices":[{"delta":{"content":null,"tool_calls":null}}]}"#]);
        assert_eq!(c, Completion::default());

        let mut acc = Accumulator::default();
        let err = acc
            .apply(r#"{"error":{"message":"overloaded"}}"#, &mut |_| {})
            .unwrap_err();
        assert!(err.0.contains("overloaded"), "{err}");
    }

    #[test]
    fn wire_messages() {
        let msgs = [
            ChatMessage::Assistant {
                content: String::new(),
                reasoning: "hidden".into(),
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "shell".into(),
                    arguments: "{}".into(),
                }],
            },
            ChatMessage::Tool {
                call_id: "c1".into(),
                content: "ok".into(),
                is_error: false,
            },
        ];
        let wire: Vec<Value> = msgs.iter().map(wire_message).collect();
        assert_eq!(
            wire,
            vec![
                json!({"role":"assistant","content":null,"tool_calls":[{"id":"c1","type":"function","function":{"name":"shell","arguments":"{}"}}]}),
                json!({"role":"tool","tool_call_id":"c1","content":"ok"}),
            ]
        );
    }
}

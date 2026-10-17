//! OpenAI-compatible `/chat/completions` streaming provider. Covers OpenAI,
//! DeepSeek, OpenRouter, MiniMax, tabbyAPI, vLLM, llama.cpp and friends.

use std::collections::BTreeMap;
use std::time::Duration;

use bone_proto::types::{ChatMessage, ReasoningState, ToolCall, Usage};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};

use super::sse::SseParser;
use super::{Completion, CompletionRequest, Delta, DeltaSink, ProviderError};
use crate::config::ProviderConfig;

const MAX_ATTEMPTS: u32 = 3;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// One completion over `/chat/completions`.
pub(super) async fn stream(
    http: &reqwest::Client,
    config: &ProviderConfig,
    req: CompletionRequest<'_>,
    on_delta: DeltaSink<'_>,
) -> Result<Completion, ProviderError> {
    let url = format!("{}/chat/completions", super::base_url(config));
    let (body, headers) = configured(request_body(config, &req), bearer(config), config);
    let mut acc = Accumulator::default();
    let done = post_events(http, &url, &headers, &body, |d| {
        acc.apply(d, &mut *on_delta)
    })
    .await?;
    acc.check_end(done)?;
    Ok(acc.finish(&config.model))
}

/// POST `body` and hand each server-sent event's data to `apply` until it
/// says the response is complete. False if the stream ended first.
pub(super) async fn post_events(
    http: &reqwest::Client,
    url: &str,
    headers: &[(String, String)],
    body: &Value,
    mut apply: impl FnMut(&str) -> Result<bool, ProviderError>,
) -> Result<bool, ProviderError> {
    let mut capture = Capture::start(url, body);
    let resp = send(http, url, headers, body, &mut capture).await?;
    let mut bytes = resp.bytes_stream();
    let mut parser = SseParser::default();
    while let Some(chunk) = bytes.next().await {
        let chunk = chunk.map_err(|e| ProviderError(format!("stream error: {e}")))?;
        capture.chunk(&chunk);
        for data in parser.push(&chunk) {
            if apply(&data)? {
                return Ok(true);
            }
        }
    }
    parser.finish().map_or(Ok(false), |data| apply(&data))
}

/// `Authorization: Bearer <key>`, when there is a key.
pub(super) fn bearer(config: &ProviderConfig) -> Vec<(String, String)> {
    let key = config.api_key.as_deref().filter(|k| !k.is_empty());
    key.map(|k| ("authorization".into(), format!("Bearer {k}")))
        .into_iter()
        .collect()
}

/// The request as the provider entry adjusts it: its `body` merged in
/// (`null` removes a field), its `headers` replacing any of the same name.
pub(super) fn configured(
    mut body: Value,
    mut headers: Vec<(String, String)>,
    config: &ProviderConfig,
) -> (Value, Vec<(String, String)>) {
    merge(&mut body, &config.body);
    for (name, value) in &config.headers {
        headers.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
        headers.push((name.clone(), value.clone()));
    }
    (body, headers)
}

fn merge(into: &mut Value, from: &Value) {
    let (Value::Object(into), Value::Object(from)) = (into, from) else {
        return;
    };
    for (k, v) in from {
        if v.is_null() {
            into.remove(k);
        } else if let Some(slot) = into.get_mut(k).filter(|s| s.is_object() && v.is_object()) {
            merge(slot, v);
        } else {
            into.insert(k.clone(), v.clone());
        }
    }
}

/// An HTTP client for providers; one serves every request a provider makes.
pub(crate) fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .expect("reqwest client builds")
}

pub(super) fn check_images(
    config: &ProviderConfig,
    messages: &[ChatMessage],
) -> Result<(), ProviderError> {
    if config.supports_images == Some(false) && messages.iter().any(|m| !m.images().is_empty()) {
        return Err(ProviderError(
            "this model does not support images; select a vision model".into(),
        ));
    }
    for message in messages {
        if message
            .images()
            .iter()
            .any(|image| image.data.as_deref().is_none_or(str::is_empty))
        {
            return Err(ProviderError(
                "image bytes are unavailable; hydrate attachments before calling the provider"
                    .into(),
            ));
        }
    }
    Ok(())
}

/// With `BONE_DEBUG_HTTP=<dir>`, each request body and raw response is saved
/// there (`<stem>.request.json`, `<stem>.response`) to replay or report.
/// Keys are sent as headers, so they are not saved.
pub(super) struct Capture(Option<std::fs::File>);

impl Capture {
    pub(super) fn start(url: &str, body: &Value) -> Self {
        let Some(dir) = std::env::var_os("BONE_DEBUG_HTTP").filter(|d| !d.is_empty()) else {
            return Capture(None);
        };
        static COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::path::PathBuf::from(dir);
        let _ = std::fs::create_dir_all(&dir);
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let n = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let stem = format!("{millis}-{}-{n}", std::process::id());
        let request = json!({ "url": url, "body": body });
        let _ = std::fs::write(
            dir.join(format!("{stem}.request.json")),
            serde_json::to_vec_pretty(&request).unwrap_or_default(),
        );
        Capture(std::fs::File::create(dir.join(format!("{stem}.response"))).ok())
    }

    pub(super) fn chunk(&mut self, bytes: &[u8]) {
        if let Some(file) = &mut self.0 {
            use std::io::Write;
            let _ = file.write_all(bytes);
        }
    }
}

/// Send the request, retrying connection failures, 429 and 5xx. Nothing has
/// streamed yet at this point, so retrying is invisible to the caller.
pub(super) async fn send(
    http: &reqwest::Client,
    url: &str,
    headers: &[(String, String)],
    body: &Value,
    capture: &mut Capture,
) -> Result<reqwest::Response, ProviderError> {
    let mut attempt = 1;
    loop {
        let mut req = http.post(url).json(body);
        for (name, value) in headers {
            req = req.header(name, value);
        }
        let retryable = match req.send().await {
            Ok(resp) if resp.status().is_success() => return Ok(resp),
            Ok(resp) => {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                capture.chunk(format!("HTTP {status}\n{text}\n").as_bytes());
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

fn request_body(config: &ProviderConfig, req: &CompletionRequest<'_>) -> Value {
    let mut body = json!({
        "model": config.model,
        "messages": wire_messages(req.messages, replay_field(req.messages, config)),
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

/// The field assistant messages carry their reasoning back in, if any: the
/// one this model streamed reasoning in, unless `replay_reasoning` forces it
/// off (or on, with `reasoning_content` before the model has streamed any).
fn replay_field<'a>(messages: &'a [ChatMessage], config: &ProviderConfig) -> Option<&'a str> {
    let streamed = messages.iter().find_map(|m| match m {
        ChatMessage::Assistant {
            reasoning_state: Some(s),
            ..
        } if s.model == config.model => s.field.as_deref(),
        _ => None,
    });
    match config.replay_reasoning {
        Some(false) => None,
        Some(true) => Some(streamed.unwrap_or("reasoning_content")),
        None => streamed,
    }
}

fn wire_messages(messages: &[ChatMessage], replay: Option<&str>) -> Vec<Value> {
    let mut out = Vec::new();
    let mut images = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        // Servers reject assistant turns with neither content nor tool calls
        // (an empty or reasoning-only reply, or a cancelled turn).
        if let ChatMessage::Assistant {
            content,
            tool_calls,
            ..
        } = message
            && content.is_empty()
            && tool_calls.is_empty()
        {
            continue;
        }
        out.push(wire_message(message, replay));
        if let ChatMessage::Tool {
            call_id,
            images: attached,
            ..
        } = message
            && !attached.is_empty()
        {
            images.push(wire_user(
                &format!("Images from tool call {call_id}"),
                attached,
            ));
        }
        // Complete all tool replies before adding their multimodal content.
        if !matches!(messages.get(index + 1), Some(ChatMessage::Tool { .. })) {
            out.append(&mut images);
        }
    }
    out
}

fn wire_message(msg: &ChatMessage, replay: Option<&str>) -> Value {
    match msg {
        ChatMessage::System { content } => json!({ "role": "system", "content": content }),
        ChatMessage::User { content, images } => wire_user(content, images),
        ChatMessage::Assistant {
            content,
            tool_calls,
            reasoning,
            ..
        } => {
            let mut m = json!({
                "role": "assistant",
                "content": if content.is_empty() { Value::Null } else { json!(content) },
            });
            if let Some(field) = replay {
                m[field] = json!(reasoning);
            }
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

fn wire_user(content: &str, images: &[bone_proto::types::ImageAttachment]) -> Value {
    if images.is_empty() {
        return json!({ "role": "user", "content": content });
    }
    let mut parts = Vec::new();
    if !content.is_empty() {
        parts.push(json!({ "type": "text", "text": content }));
    }
    for image in images {
        parts.push(json!({ "type": "image_url", "image_url": {
            "url": format!("data:{};base64,{}", image.mime_type, image.data.as_deref().unwrap_or_default())
        }}));
    }
    json!({ "role": "user", "content": parts })
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
    #[serde(default)]
    finish_reason: Option<String>,
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
    #[serde(default)]
    prompt_tokens_details: Option<PromptTokensDetails>,
    #[serde(default)]
    prompt_cache_hit_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct PromptTokensDetails {
    #[serde(default)]
    cached_tokens: Option<u64>,
}

#[derive(Default)]
struct Accumulator {
    content: String,
    reasoning: String,
    /// The field reasoning streamed in.
    reasoning_field: Option<&'static str>,
    calls: BTreeMap<usize, ToolCall>,
    usage: Option<Usage>,
    finish_reason: Option<String>,
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
                context_tokens: None,
                cached_tokens: u
                    .prompt_tokens_details
                    .and_then(|d| d.cached_tokens)
                    .or(u.prompt_cache_hit_tokens),
            });
        }
        let choices = chunk.choices.unwrap_or_default();
        if let Some(reason) = choices.iter().find_map(|c| c.finish_reason.clone()) {
            self.finish_reason = Some(reason);
        }
        let deltas = choices.into_iter().filter_map(|c| c.delta);
        for d in deltas {
            if let Some((field, text)) = [
                ("reasoning_content", d.reasoning_content),
                ("reasoning", d.reasoning),
            ]
            .into_iter()
            .find_map(|(f, t)| t.filter(|t| !t.is_empty()).map(|t| (f, t)))
            {
                self.reasoning_field.get_or_insert(field);
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

    /// Whether a stream that stopped (`done`: at `[DONE]`) gave a whole
    /// answer. Some servers end without `[DONE]` but after a finish reason.
    fn check_end(&self, done: bool) -> Result<(), ProviderError> {
        let why = match self.finish_reason.as_deref() {
            Some("length") => "response incomplete: the output token limit was reached",
            Some("content_filter") => "the response was stopped by the content filter",
            None if !done => "the stream ended before the response completed",
            _ => return Ok(()),
        };
        Err(ProviderError(why.into()))
    }

    fn finish(self, model: &str) -> Completion {
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
            reasoning_state: self.reasoning_field.map(|f| ReasoningState {
                model: model.to_owned(),
                items: Vec::new(),
                prefix: None,
                field: Some(f.into()),
            }),
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
        (acc.finish("m"), deltas)
    }

    #[test]
    fn accumulates_text_reasoning_and_tool_calls() {
        let (c, deltas) = run(&[
            r#"{"choices":[{"delta":{"role":"assistant","reasoning_content":"","reasoning":"think"}}]}"#,
            r#"{"choices":[{"delta":{"content":"Hi"}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"read_file","arguments":"{\"pa"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"a\"}"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"name":"shell","arguments":"{}"}}]}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}],"usage":null}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"prompt_tokens_details":{"cached_tokens":6}}}"#,
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
                output_tokens: 5,
                context_tokens: None,
                cached_tokens: Some(6),
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
                reasoning_state: None,
            },
            ChatMessage::Tool {
                images: Vec::new(),
                call_id: "c1".into(),
                content: "ok".into(),
                is_error: false,
            },
        ];
        let wire: Vec<Value> = msgs.iter().map(|msg| wire_message(msg, None)).collect();
        assert_eq!(
            wire,
            vec![
                json!({"role":"assistant","content":null,"tool_calls":[{"id":"c1","type":"function","function":{"name":"shell","arguments":"{}"}}]}),
                json!({"role":"tool","tool_call_id":"c1","content":"ok"}),
            ]
        );
        let mut replayed = wire.clone();
        replayed[0]["reasoning_content"] = json!("hidden");
        assert_eq!(
            msgs.iter()
                .map(|msg| wire_message(msg, Some("reasoning_content")))
                .collect::<Vec<_>>(),
            replayed
        );
        for reasoning in ["", "thought"] {
            let msg = ChatMessage::Assistant {
                content: "answer".into(),
                reasoning: reasoning.into(),
                tool_calls: vec![],
                reasoning_state: None,
            };
            assert_eq!(
                wire_message(&msg, Some("reasoning_content")),
                json!({
                    "role":"assistant", "content":"answer", "reasoning_content":reasoning
                })
            );
            assert!(wire_message(&msg, None).get("reasoning_content").is_none());
        }
    }
}

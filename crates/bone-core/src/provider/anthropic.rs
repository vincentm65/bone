//! Anthropic's Messages API (`/messages`): streaming, prompt-cache
//! breakpoints, images, and thinking.
//!
//! Thinking blocks go back to the model unchanged, but Anthropic accepts
//! them only while everything before them (system, tools, earlier messages)
//! is byte-for-byte what it was when they were written. Each assistant
//! message keeps a fingerprint of that history; once the history differs
//! (compaction, a new system prompt, a tool added) its thinking is left out
//! instead of failing the request.

use std::collections::BTreeMap;

use bone_proto::types::{ChatMessage, ImageAttachment, ReasoningState, ToolCall, Usage};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::openai::{configured, post_events};
use super::{Completion, CompletionRequest, Delta, DeltaSink, ProviderError};
use crate::config::ProviderConfig;

pub(crate) const DEFAULT_URL: &str = "https://api.anthropic.com/v1";
const DEFAULT_MAX_TOKENS: u64 = 32_000;

/// One completion over Anthropic's `/messages`.
pub(super) async fn stream(
    http: &reqwest::Client,
    config: &ProviderConfig,
    req: CompletionRequest<'_>,
    on_delta: DeltaSink<'_>,
) -> Result<Completion, ProviderError> {
    let url = format!("{}/messages", super::base_url(config));
    let (body, prefix) = request_body(config, &req);
    let mut headers = vec![("anthropic-version".into(), "2023-06-01".into())];
    if let Some(key) = config.api_key.as_deref().filter(|k| !k.is_empty()) {
        headers.push(("x-api-key".into(), key.to_owned()));
    }
    let (body, headers) = configured(body, headers, config);
    let mut acc = Accumulator::default();
    if !post_events(http, &url, &headers, &body, |d| {
        acc.apply(d, &mut *on_delta)
    })
    .await?
    {
        return Err(ProviderError(
            "the stream ended before the response completed".into(),
        ));
    }
    acc.finish(&config.model, prefix)
}

/// The request, and the fingerprint of its history for the reply's thinking.
fn request_body(config: &ProviderConfig, req: &CompletionRequest<'_>) -> (Value, String) {
    // Cache breakpoints unless the entry says `cache = false`.
    let cache = config.options.get("cache") != Some(&Value::Bool(false));
    let effort = config
        .reasoning_effort
        .as_deref()
        .filter(|e| !e.is_empty() && *e != "default");
    // Effort "none", or `body = { thinking = null }`, turns thinking off.
    let replay = effort != Some("none") && !config.body.get("thinking").is_some_and(Value::is_null);
    let mut history = History::default();
    // The open tool loop's thinking could not go back.
    let mut lost = false;
    let mut tools: Vec<Value> = req
        .tools
        .iter()
        .map(|t| json!({ "name": t.name, "description": t.description, "input_schema": t.parameters }))
        .collect();
    let mut system = Vec::new();
    let mut messages: Vec<Value> = Vec::new();
    for msg in req.messages {
        let (role, mut blocks) = match msg {
            ChatMessage::System { content } => {
                system.extend(text(content));
                continue;
            }
            ChatMessage::User { content, images } => {
                let blocks = images.iter().map(image).chain(text(content)).collect();
                ("user", blocks)
            }
            ChatMessage::Tool {
                call_id,
                content,
                images,
                is_error,
            } => {
                let mut block = json!({ "type": "tool_result", "tool_use_id": call_id });
                let parts: Vec<Value> = text(content)
                    .into_iter()
                    .chain(images.iter().map(image))
                    .collect();
                if !parts.is_empty() {
                    block["content"] = json!(parts);
                }
                if *is_error {
                    block["is_error"] = json!(true);
                }
                ("user", vec![block])
            }
            ChatMessage::Assistant {
                content,
                tool_calls,
                reasoning_state,
                ..
            } => {
                // Thinking goes back only to its model, in its conversation.
                let fingerprint = history.fingerprint(&system, &tools, &messages);
                let state = reasoning_state.as_ref().filter(|s| {
                    replay
                        && s.model == config.model
                        && s.prefix.as_deref() == Some(fingerprint.as_str())
                });
                let had = matches!(&reasoning_state, Some(s) if !s.items.is_empty());
                lost = had && state.is_none() && !tool_calls.is_empty();
                ("assistant", assistant_blocks(content, tool_calls, state))
            }
        };
        if blocks.is_empty() {
            continue;
        }
        // Tool results share one user turn: merge adjacent roles.
        match messages.last_mut().filter(|m| m["role"] == role) {
            Some(last) => last["content"].as_array_mut().unwrap().append(&mut blocks),
            None => messages.push(json!({ "role": role, "content": blocks })),
        }
    }
    let fingerprint = history.fingerprint(&system, &tools, &messages);
    // Thinking stays on unless the tool loop under way lost its own.
    let thinking =
        replay && !(lost && matches!(req.messages.last(), Some(ChatMessage::Tool { .. })));
    if cache {
        breakpoints(&mut tools, &mut system, &mut messages);
    }
    let mut body = json!({
        "model": config.model,
        "max_tokens": DEFAULT_MAX_TOKENS,
        "stream": true,
        "messages": messages,
    });
    if !system.is_empty() {
        body["system"] = json!(system);
    }
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }
    if thinking {
        body["thinking"] = json!({ "type": "adaptive", "display": "summarized" });
    }
    if let Some(effort) = effort.filter(|e| *e != "none") {
        body["output_config"] = json!({ "effort": effort });
    }
    (body, fingerprint)
}

/// A text block, unless the text is blank (Anthropic rejects those).
fn text(text: &str) -> Option<Value> {
    (!text.trim().is_empty()).then(|| json!({ "type": "text", "text": text }))
}

fn image(image: &ImageAttachment) -> Value {
    json!({ "type": "image", "source": {
        "type": "base64", "media_type": image.mime_type,
        "data": image.data.as_deref().unwrap_or_default(),
    }})
}

fn tool_use(call: &ToolCall) -> Value {
    // Arguments the model got wrong still go back, as something valid.
    let input = serde_json::from_str::<Value>(&call.arguments)
        .ok()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({ "invalid_arguments": call.arguments }));
    json!({ "type": "tool_use", "id": call.id, "name": call.name, "input": input })
}

/// An assistant message's blocks. With saved thinking, the saved blocks go
/// back in their order, unchanged where the message still matches them (a
/// hook may have edited its text or calls).
fn assistant_blocks(
    content: &str,
    tool_calls: &[ToolCall],
    state: Option<&ReasoningState>,
) -> Vec<Value> {
    let mut out = Vec::new();
    let mut sent_text = content.trim().is_empty();
    let mut sent_calls = vec![false; tool_calls.len()];
    for item in state.map(|s| s.items.as_slice()).unwrap_or_default() {
        match item["type"].as_str() {
            Some("thinking" | "redacted_thinking") => out.push(item.clone()),
            Some("text") if !sent_text => {
                sent_text = true;
                out.push(if item["text"] == content {
                    item.clone()
                } else {
                    json!({ "type": "text", "text": content })
                });
            }
            Some("tool_use") => {
                let Some(i) = tool_calls
                    .iter()
                    .position(|c| item["id"].as_str() == Some(&c.id))
                else {
                    continue;
                };
                sent_calls[i] = true;
                let call = tool_use(&tool_calls[i]);
                out.push(
                    if call["input"] == item["input"] && call["name"] == item["name"] {
                        item.clone()
                    } else {
                        call
                    },
                );
            }
            _ => {}
        }
    }
    // Thinking that led to nothing left cannot stand alone.
    while out.last().is_some_and(is_thinking) {
        out.pop();
    }
    if !sent_text {
        let at = out
            .iter()
            .position(|b| b["type"] == "tool_use")
            .unwrap_or(out.len());
        out.insert(at, json!({ "type": "text", "text": content }));
    }
    for (call, sent) in tool_calls.iter().zip(sent_calls) {
        if !sent {
            out.push(tool_use(call));
        }
    }
    out
}

fn is_thinking(block: &Value) -> bool {
    matches!(
        block["type"].as_str(),
        Some("thinking" | "redacted_thinking")
    )
}

/// Fingerprints of what precedes each message. A finished message is
/// serialized once; only the last, which may still grow by merging, again.
#[derive(Default)]
struct History {
    /// System and tools, hashed again only when a system message adds to them.
    head: Option<(usize, [u8; 32])>,
    done: Vec<[u8; 32]>,
}

impl History {
    fn fingerprint(&mut self, system: &[Value], tools: &[Value], messages: &[Value]) -> String {
        let done = messages.len().saturating_sub(1);
        while self.done.len() < done {
            let next = &messages[self.done.len()];
            self.done.push(Sha256::digest(next.to_string()).into());
        }
        if self.head.is_none_or(|(n, _)| n != system.len()) {
            let head = Sha256::digest(json!([system, tools]).to_string()).into();
            self.head = Some((system.len(), head));
        }
        let mut h = Sha256::new();
        h.update(self.head.unwrap().1);
        self.done.iter().for_each(|d| h.update(d));
        h.update(json!(messages.get(done)).to_string());
        h.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// Mark up to four cache breakpoints: the tools, the system prompt, the end
/// of the previous turn and the newest block. The previous turn's mark keeps
/// last request's cache in reach even when this turn added more than the 20
/// blocks a breakpoint looks back.
fn breakpoints(tools: &mut [Value], system: &mut [Value], messages: &mut [Value]) {
    let mark = |block: &mut Value| block["cache_control"] = json!({ "type": "ephemeral" });
    if let Some(last) = tools.last_mut() {
        mark(last);
    }
    if let Some(last) = system.last_mut() {
        mark(last);
    }
    let previous = messages
        .iter()
        .rposition(|m| m["role"] == "assistant")
        .and_then(|i| i.checked_sub(1));
    for i in previous.into_iter().chain(messages.len().checked_sub(1)) {
        // Thinking blocks cannot carry a breakpoint.
        let block = messages[i]["content"]
            .as_array_mut()
            .and_then(|blocks| blocks.iter_mut().rev().find(|b| !is_thinking(b)));
        if let Some(block) = block {
            mark(block);
        }
    }
}

// ---- stream events -------------------------------------------------------

enum Block {
    Thinking {
        text: String,
        signature: String,
    },
    Redacted(Value),
    Text(String),
    ToolUse {
        id: String,
        name: String,
        json: String,
        initial: Value,
    },
}

#[derive(Default)]
struct Accumulator {
    blocks: BTreeMap<u64, Block>,
    content: String,
    reasoning: String,
    usage: Option<Usage>,
    input_tokens: u64,
    cache_read: u64,
    cache_written: u64,
    stop_reason: Option<String>,
    started: bool,
}

impl Accumulator {
    /// Apply one SSE data payload. Returns true at `message_stop`.
    fn apply(&mut self, data: &str, on_delta: DeltaSink<'_>) -> Result<bool, ProviderError> {
        let ev: Value = serde_json::from_str(data)
            .map_err(|e| ProviderError(format!("bad stream event ({e}): {data}")))?;
        let index = ev["index"].as_u64().unwrap_or_default();
        match ev["type"].as_str().unwrap_or_default() {
            "error" => return Err(ProviderError(format!("provider error: {}", ev["error"]))),
            "message_start" => {
                self.started = true;
                self.update_usage(&ev["message"]["usage"]);
            }
            "content_block_start" => {
                let b = &ev["content_block"];
                let block = match b["type"].as_str().unwrap_or_default() {
                    "thinking" => {
                        if !self.reasoning.is_empty() {
                            self.reasoning(String::from("\n\n"), on_delta);
                        }
                        let text = b["thinking"].as_str().unwrap_or_default().to_owned();
                        self.reasoning(text.clone(), on_delta);
                        Block::Thinking {
                            text,
                            signature: b["signature"].as_str().unwrap_or_default().into(),
                        }
                    }
                    "redacted_thinking" => Block::Redacted(b.clone()),
                    "text" => {
                        let text = b["text"].as_str().unwrap_or_default().to_owned();
                        self.text(text.clone(), on_delta);
                        Block::Text(text)
                    }
                    "tool_use" => Block::ToolUse {
                        id: b["id"].as_str().unwrap_or_default().into(),
                        name: b["name"].as_str().unwrap_or_default().into(),
                        json: String::new(),
                        initial: b.get("input").cloned().unwrap_or_else(|| json!({})),
                    },
                    // Unknown kinds (server tools) are not ours to replay.
                    _ => return Ok(false),
                };
                self.blocks.insert(index, block);
            }
            "content_block_delta" => {
                let d = &ev["delta"];
                let piece = |k: &str| d[k].as_str().unwrap_or_default().to_owned();
                match (d["type"].as_str(), self.blocks.get_mut(&index)) {
                    (Some("text_delta"), Some(Block::Text(text))) => {
                        let t = piece("text");
                        text.push_str(&t);
                        self.text(t, on_delta);
                    }
                    (Some("thinking_delta"), Some(Block::Thinking { text, .. })) => {
                        let t = piece("thinking");
                        text.push_str(&t);
                        self.reasoning(t, on_delta);
                    }
                    (Some("signature_delta"), Some(Block::Thinking { signature, .. })) => {
                        signature.push_str(&piece("signature"));
                    }
                    (Some("input_json_delta"), Some(Block::ToolUse { json, .. })) => {
                        json.push_str(&piece("partial_json"));
                    }
                    _ => {}
                }
            }
            "message_delta" => {
                if let Some(reason) = ev["delta"]["stop_reason"].as_str() {
                    self.stop_reason = Some(reason.into());
                }
                self.update_usage(&ev["usage"]);
            }
            "message_stop" => {
                if !self.started {
                    return Err(ProviderError("message_stop without message_start".into()));
                }
                return Ok(true);
            }
            _ => {} // ping, content_block_stop, and newer event types
        }
        Ok(false)
    }

    fn text(&mut self, text: String, on_delta: DeltaSink<'_>) {
        if !text.is_empty() {
            self.content.push_str(&text);
            on_delta(Delta::Text(text));
        }
    }

    fn reasoning(&mut self, text: String, on_delta: DeltaSink<'_>) {
        if !text.is_empty() {
            self.reasoning.push_str(&text);
            on_delta(Delta::Reasoning(text));
        }
    }

    /// Usage fields arrive cumulatively; each event replaces what it names.
    fn update_usage(&mut self, usage: &Value) {
        if let Some(n) = usage["input_tokens"].as_u64() {
            self.input_tokens = n;
        }
        if let Some(n) = usage["cache_read_input_tokens"].as_u64() {
            self.cache_read = n;
        }
        if let Some(n) = usage["cache_creation_input_tokens"].as_u64() {
            self.cache_written = n;
        }
        let u = self.usage.get_or_insert_with(Usage::default);
        u.input_tokens = self.input_tokens + self.cache_read + self.cache_written;
        u.cached_tokens = Some(self.cache_read);
        if let Some(n) = usage["output_tokens"].as_u64() {
            u.output_tokens = n;
        }
    }

    fn finish(self, model: &str, prefix: String) -> Result<Completion, ProviderError> {
        let why = match self.stop_reason.as_deref() {
            Some("max_tokens") => "response incomplete: max_tokens was reached",
            // Worded for the overflow check, which compacts and retries.
            Some("model_context_window_exceeded") => "the context window is full",
            Some("refusal") => "the model declined to answer (refusal)",
            _ => "",
        };
        if !why.is_empty() {
            return Err(ProviderError(why.into()));
        }
        let mut items = Vec::new();
        let mut tool_calls = Vec::new();
        for block in self.blocks.into_values() {
            match block {
                Block::Thinking { text, signature } => items
                    .push(json!({ "type": "thinking", "thinking": text, "signature": signature })),
                Block::Redacted(b) => items.push(b),
                Block::Text(text) => items.push(json!({ "type": "text", "text": text })),
                Block::ToolUse {
                    id,
                    name,
                    json,
                    initial,
                } => {
                    let input = if json.is_empty() {
                        initial
                    } else {
                        serde_json::from_str(&json).unwrap_or(Value::String(json))
                    };
                    let arguments = match &input {
                        Value::String(raw) => raw.clone(),
                        v => v.to_string(),
                    };
                    items.push(
                        json!({ "type": "tool_use", "id": id, "name": name, "input": input }),
                    );
                    tool_calls.push(ToolCall {
                        id,
                        name,
                        arguments,
                    });
                }
            }
        }
        let keep = items.iter().any(is_thinking);
        Ok(Completion {
            content: self.content,
            reasoning: self.reasoning,
            tool_calls,
            usage: self.usage,
            reasoning_state: keep.then(|| ReasoningState {
                model: model.to_owned(),
                items,
                prefix: Some(prefix),
                field: None,
            }),
        })
    }
}

#[cfg(test)]
mod tests;

//! Anthropic Messages API provider with native prompt caching.
//!
//! Unlike the OpenAI-compatible provider this speaks Anthropic's own wire
//! format: a top-level `system` array, typed content blocks, `tool_use` /
//! `tool_result` blocks, and the `content_block_delta` / `message_delta` SSE
//! event stream. It reuses `streaming_client`, `http_error`, and the
//! reasoning-agnostic tool-call accumulation helpers from `openai_compat`.
//!
//! Caching: a single `cache_control: {type: "ephemeral"}` breakpoint is placed
//! on the last system block. Anthropic renders tools -> system -> messages, so
//! this one breakpoint caches the entire stable prefix (tool schemas + system
//! prompt) that the driver already keeps constant across turns.

use async_stream::try_stream;
use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures_util::TryStreamExt;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

use crate::config::ProviderEntry;
use crate::llm::provider::{
    ChatEvent, ChatMessage, ChatRole, LlmError, LlmErrorKind, LlmProvider, ResponseStream,
    http_error, parse_tool_arguments, streaming_client,
};
use crate::tools::{ToolCall, ToolDefinition};

const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Anthropic requires `max_tokens`; use a generous default when unset.
const DEFAULT_MAX_TOKENS: u32 = 18000;

pub struct AnthropicProvider {
    client: reqwest::Client,
    base_url: String,
    model: String,
    api_key: String,
    endpoint: String,
    id: String,
    label: String,
    max_tokens: Option<u32>,
    context_window_tokens: Option<u64>,
}

impl AnthropicProvider {
    pub fn from_entry(id: &str, entry: &ProviderEntry) -> Self {
        let label = if entry.label.is_empty() {
            id.to_string()
        } else {
            entry.label.clone()
        };
        Self {
            client: streaming_client(),
            id: id.to_string(),
            label,
            base_url: entry.base_url.trim_end_matches('/').to_string(),
            model: entry.model.clone(),
            api_key: entry.api_key.resolve_or_warn(),
            endpoint: entry.endpoint.clone(),
            max_tokens: None,
            context_window_tokens: entry.context_window_tokens,
        }
    }

    fn messages_url(&self) -> String {
        format!("{}{}", self.base_url, self.endpoint)
    }
}

// --- Request wire types -----------------------------------------------------

#[derive(Serialize)]
struct MessagesRequest {
    model: String,
    max_tokens: u32,
    stream: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    system: Vec<SystemBlock>,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<AnthropicTool>,
}

#[derive(Serialize)]
struct SystemBlock {
    r#type: &'static str,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<CacheControl>,
}

#[derive(Serialize)]
struct CacheControl {
    r#type: &'static str,
}

impl CacheControl {
    fn ephemeral() -> Self {
        Self {
            r#type: "ephemeral",
        }
    }
}

#[derive(Serialize)]
struct AnthropicMessage {
    role: &'static str,
    content: Vec<Value>,
}

#[derive(Serialize)]
struct AnthropicTool {
    name: String,
    description: String,
    input_schema: Value,
}

/// Split provider-neutral messages into the top-level `system` array and the
/// role-tagged `messages` array. System messages are concatenated into system
/// blocks; the last one carries the cache breakpoint. Tool results become
/// `user` messages with `tool_result` blocks (Anthropic has no `tool` role).
fn build_request_parts(messages: Vec<ChatMessage>) -> (Vec<SystemBlock>, Vec<AnthropicMessage>) {
    let mut system = Vec::new();
    let mut out = Vec::new();

    for message in messages {
        match message.role {
            ChatRole::System => {
                if !message.content.is_empty() {
                    system.push(SystemBlock {
                        r#type: "text",
                        text: message.content,
                        cache_control: None,
                    });
                }
            }
            ChatRole::Tool => {
                // A tool result is carried on a user-role message as a
                // `tool_result` block keyed by the originating tool_use id.
                let mut block = json!({
                    "type": "tool_result",
                    "tool_use_id": message.tool_call_id.clone().unwrap_or_default(),
                    "content": message.content,
                });
                if message.is_error {
                    block["is_error"] = json!(true);
                }
                out.push(AnthropicMessage {
                    role: "user",
                    content: vec![block],
                });
            }
            ChatRole::User => {
                let mut content = Vec::new();
                if !message.content.is_empty() {
                    content.push(json!({ "type": "text", "text": message.content }));
                }
                for image in message.images {
                    content.push(json!({
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": image.media_type,
                            "data": image.data,
                        }
                    }));
                }
                if content.is_empty() {
                    content.push(json!({ "type": "text", "text": "" }));
                }
                out.push(AnthropicMessage {
                    role: "user",
                    content,
                });
            }
            ChatRole::Assistant => {
                let mut content = Vec::new();
                if !message.content.is_empty() {
                    content.push(json!({ "type": "text", "text": message.content }));
                }
                for call in message.tool_calls {
                    content.push(json!({
                        "type": "tool_use",
                        "id": call.id,
                        "name": call.name,
                        "input": call.arguments,
                    }));
                }
                if content.is_empty() {
                    continue;
                }
                out.push(AnthropicMessage {
                    role: "assistant",
                    content,
                });
            }
        }
    }

    // Cache the whole stable prefix (tools + system) at the last system block.
    if let Some(last) = system.last_mut() {
        last.cache_control = Some(CacheControl::ephemeral());
    }

    (system, out)
}

fn anthropic_tools(tools: Vec<ToolDefinition>) -> Vec<AnthropicTool> {
    tools
        .into_iter()
        .map(|tool| AnthropicTool {
            name: tool.name,
            description: tool.description,
            input_schema: tool.input_schema,
        })
        .collect()
}

/// Accumulates a streaming `tool_use` block: the block header carries the id
/// and name, and `input_json_delta` events append the argument JSON text.
#[derive(Default)]
struct PartialToolUse {
    id: String,
    name: String,
    input: String,
}

fn finish_tool_use(partial: PartialToolUse) -> Option<ChatEvent> {
    if partial.id.is_empty() || partial.name.is_empty() {
        return None;
    }
    Some(ChatEvent::ToolCall(ToolCall {
        id: partial.id,
        name: partial.name,
        arguments: parse_tool_arguments(&partial.input),
    }))
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.label
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn set_model(&mut self, model: String) {
        self.model = model;
    }

    fn set_max_tokens(&mut self, max_tokens: Option<u32>) {
        self.max_tokens = max_tokens;
    }

    fn context_window_tokens(&self) -> Option<u64> {
        self.context_window_tokens
    }

    async fn chat_stream(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<ResponseStream, LlmError> {
        let (system, msgs) = build_request_parts(messages);
        let request = MessagesRequest {
            model: self.model.clone(),
            max_tokens: self.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
            stream: true,
            system,
            messages: msgs,
            tools: anthropic_tools(tools),
        };

        let response = self
            .client
            .post(self.messages_url())
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&request)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            return Err(http_error(status, &self.messages_url(), &error_body));
        }

        let events = response.bytes_stream().eventsource();

        let stream = try_stream! {
            futures_util::pin_mut!(events);
            // Anthropic streams one content block at a time, so a single
            // in-flight `tool_use` accumulator (keyed by block index) suffices.
            let mut partials: BTreeMap<u64, PartialToolUse> = BTreeMap::new();
            let mut prompt_tokens = 0u32;
            let mut cached_tokens: Option<u32> = None;
            let mut completion_tokens = 0u32;

            while let Some(event) = events.try_next().await.map_err(|err| {
                LlmError::new_with_kind(LlmErrorKind::Connection, err.to_string())
            })? {
                let data = event.data.trim();
                if data.is_empty() {
                    continue;
                }
                let value: Value = serde_json::from_str(data)?;
                let kind = value.get("type").and_then(|t| t.as_str()).unwrap_or("");

                match kind {
                    "message_start" => {
                        // Initial usage: input tokens and cache reads.
                        if let Some(usage) = value.pointer("/message/usage") {
                            prompt_tokens = usage_input_tokens(usage);
                            cached_tokens = usage
                                .get("cache_read_input_tokens")
                                .and_then(|v| v.as_u64())
                                .map(|v| v as u32);
                        }
                    }
                    "content_block_start" => {
                        let index = value.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
                        if let Some(block) = value.get("content_block")
                            && block.get("type").and_then(|t| t.as_str()) == Some("tool_use")
                        {
                            partials.insert(index, PartialToolUse {
                                id: block.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                                name: block.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                                input: String::new(),
                            });
                        }
                    }
                    "content_block_delta" => {
                        let index = value.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
                        let Some(delta) = value.get("delta") else { continue };
                        match delta.get("type").and_then(|t| t.as_str()) {
                            Some("text_delta") => {
                                if let Some(text) = delta.get("text").and_then(|v| v.as_str()) {
                                    yield ChatEvent::TextDelta(text.to_string());
                                }
                            }
                            Some("input_json_delta") => {
                                if let Some(partial) = delta.get("partial_json").and_then(|v| v.as_str())
                                    && let Some(p) = partials.get_mut(&index)
                                {
                                    p.input.push_str(partial);
                                }
                            }
                            _ => {}
                        }
                    }
                    "content_block_stop" => {
                        let index = value.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
                        if let Some(partial) = partials.remove(&index)
                            && let Some(event) = finish_tool_use(partial)
                        {
                            yield event;
                        }
                    }
                    "message_delta" => {
                        // Output-token count arrives with the final delta.
                        if let Some(out) = value
                            .pointer("/usage/output_tokens")
                            .and_then(|v| v.as_u64())
                        {
                            completion_tokens = out as u32;
                        }
                    }
                    "message_stop" => {
                        yield ChatEvent::TokenUsage {
                            prompt_tokens,
                            completion_tokens,
                            cached_tokens,
                            cost: None,
                        };
                    }
                    "error" => {
                        let msg = value
                            .pointer("/error/message")
                            .and_then(|v| v.as_str())
                            .unwrap_or("anthropic stream error");
                        Err(LlmError::new_with_kind(LlmErrorKind::Server(0), msg.to_string()))?;
                    }
                    _ => {}
                }
            }
        };

        Ok(Box::pin(stream))
    }
}

/// Anthropic's `input_tokens` counts only the *uncached* input; cache reads are
/// reported separately. Sum them so `prompt_tokens` reflects the full prompt,
/// matching how the OpenAI-compat providers report `prompt_tokens`.
fn usage_input_tokens(usage: &Value) -> u32 {
    let base = usage
        .get("input_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let cache_read = usage
        .get("cache_read_input_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let cache_creation = usage
        .get("cache_creation_input_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    (base + cache_read + cache_creation) as u32
}

#[cfg(test)]
#[path = "anthropic_tests.rs"]
mod anthropic_tests;

//! OpenAI-compatible Chat Completions provider with tool-call and reasoning support.

use async_stream::try_stream;
use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures_util::TryStreamExt;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

use crate::config::ProviderEntry;
use crate::llm::provider::{
    ChatEvent, ChatMessage, ChatRole, LlmError, LlmErrorKind, LlmProvider, ResponseStream,
    http_status_to_error_kind,
};
use crate::tools::{ToolCall, ToolDefinition};

/// Generic OpenAI-compatible provider for any server with a `/chat/completions`
/// streaming endpoint: llama.cpp, OpenRouter, GLM, Gemini, Kimi, DeepSeek, etc.
/// Set `endpoint` in config to control the path (default: `/chat/completions`).
pub struct OpenAiCompatProvider {
    client: reqwest::Client,
    base_url: String,
    model: String,
    api_key: String,
    endpoint: String,
    id: String,
    label: String,
    /// Optional cap on output tokens, sent as `max_tokens`. `None` omits the
    /// field so the server applies its own default.
    max_tokens: Option<u32>,
}

impl OpenAiCompatProvider {
    pub fn from_entry(id: &str, entry: &ProviderEntry) -> Self {
        let label = if entry.label.is_empty() {
            id.to_string()
        } else {
            entry.label.clone()
        };
        Self {
            client: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(10))
                // Idle (between-chunks) timeout, NOT a total-request timeout. A
                // reasoning model on a long prompt can legitimately stream for
                // many minutes; a total `.timeout()` would kill the whole turn
                // mid-think. `read_timeout` instead only trips when the stream
                // genuinely stalls (dropped connection), which is what we want.
                .read_timeout(std::time::Duration::from_secs(120))
                .build()
                .unwrap_or_default(),
            id: id.to_string(),
            label,
            base_url: entry.base_url.trim_end_matches('/').to_string(),
            model: entry.model.clone(),
            api_key: entry.api_key.clone(),
            endpoint: entry.endpoint.clone(),
            max_tokens: None,
        }
    }

    fn chat_url(&self) -> String {
        format!("{}{}", self.base_url, self.endpoint)
    }
}

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<OpenAiMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<OpenAiTool>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    stream_options: Option<StreamOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

/// Message `content` on the wire: either a plain string or, for multimodal
/// messages, an array of typed parts (`text` / `image_url`).
#[derive(Serialize)]
#[serde(untagged)]
enum OaiContent {
    Text(String),
    Parts(Vec<OaiPart>),
}

#[derive(Serialize)]
#[serde(tag = "type")]
enum OaiPart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image_url")]
    ImageUrl { image_url: OaiImageUrl },
}

#[derive(Serialize)]
struct OaiImageUrl {
    url: String,
}

#[derive(Serialize)]
pub(crate) struct OpenAiMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<OaiContent>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<OpenAiToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    /// Reasoning echoed back under its provider-specific wire key (e.g.
    /// DeepSeek's `reasoning_content`, MiniMax's `thoughts`). Some providers
    /// 400 if it is dropped when the turn involved tool calls.
    #[serde(flatten, skip_serializing_if = "BTreeMap::is_empty")]
    reasoning: BTreeMap<String, String>,
}

#[derive(Serialize)]
struct OpenAiTool {
    r#type: &'static str,
    function: OpenAiFunction,
}

#[derive(Serialize)]
struct OpenAiFunction {
    name: String,
    description: String,
    parameters: Value,
}

#[derive(Serialize)]
struct OpenAiToolCall {
    id: String,
    r#type: &'static str,
    function: OpenAiToolCallFunction,
}

#[derive(Serialize)]
struct OpenAiToolCallFunction {
    name: String,
    arguments: String,
}

#[derive(Debug, Default)]
pub struct PartialToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

fn openai_tools(tools: Vec<ToolDefinition>) -> Vec<OpenAiTool> {
    tools
        .into_iter()
        .map(|tool| OpenAiTool {
            r#type: "function",
            function: OpenAiFunction {
                name: tool.name.to_string(),
                description: tool.description.to_string(),
                parameters: tool.input_schema,
            },
        })
        .collect()
}

pub(crate) fn openai_messages(messages: Vec<ChatMessage>) -> Vec<OpenAiMessage> {
    messages
        .into_iter()
        .map(|message| OpenAiMessage {
            role: match message.role {
                ChatRole::System => "system",
                ChatRole::User => "user",
                ChatRole::Assistant => "assistant",
                ChatRole::Tool => "tool",
            }
            .to_string(),
            content: if !message.images.is_empty() {
                // Multimodal: a leading text part (when present) followed by one
                // image_url part per attachment, as a base64 data URL.
                let mut parts = Vec::new();
                if !message.content.is_empty() {
                    parts.push(OaiPart::Text {
                        text: message.content,
                    });
                }
                for image in message.images {
                    parts.push(OaiPart::ImageUrl {
                        image_url: OaiImageUrl {
                            url: format!("data:{};base64,{}", image.media_type, image.data),
                        },
                    });
                }
                Some(OaiContent::Parts(parts))
            } else if message.content.is_empty() && !message.tool_calls.is_empty() {
                None
            } else {
                Some(OaiContent::Text(message.content))
            },
            tool_calls: message
                .tool_calls
                .into_iter()
                .map(|call| OpenAiToolCall {
                    id: call.id,
                    r#type: "function",
                    function: OpenAiToolCallFunction {
                        name: call.name,
                        arguments: call.arguments.to_string(),
                    },
                })
                .collect(),
            tool_call_id: message.tool_call_id,
            name: message.name,
            reasoning: match message.reasoning {
                Some(crate::llm::Reasoning {
                    text,
                    echo_field: Some(key),
                }) => BTreeMap::from([(key, text)]),
                _ => BTreeMap::new(),
            },
        })
        .collect()
}

/// Flush accumulated partial tool calls, emitting a [`ChatEvent::ToolCall`]
/// for each complete entry (id and name must be non-empty).
pub fn flush_partial_tool_calls(
    partial_tool_calls: &mut BTreeMap<usize, PartialToolCall>,
) -> Vec<ChatEvent> {
    let completed = std::mem::take(partial_tool_calls);
    let mut events = Vec::new();
    for (_, call) in completed {
        if call.id.is_empty() || call.name.is_empty() {
            continue;
        }
        let arguments = serde_json::from_str(&call.arguments).unwrap_or(Value::Null);
        events.push(ChatEvent::ToolCall(ToolCall {
            id: call.id,
            name: call.name,
            arguments,
        }));
    }
    events
}

const THINK_OPEN: &str = "<think>";
const THINK_CLOSE: &str = "</think>";

/// Length of the longest suffix of `text` that equals a prefix of `tag`
/// (1..tag.len()). Zero when there is no partial match. Used to hold back
/// bytes that might be the start of a `<think>`/`</think>` tag split across
/// streaming chunks.
fn partial_tag_suffix_len(text: &str, tag: &str) -> usize {
    (1..tag.len())
        .rev()
        .find(|&n| text.ends_with(&tag[..n]))
        .unwrap_or(0)
}

/// Advance past any `\n`/`\r` bytes at `pos`. Providers commonly emit
/// `<think>\n…` and `…</think>\n\n`; the newlines immediately adjacent to a
/// tag are never meaningful content, so strip them so neither the reasoning
/// nor the answer starts with a blank line.
fn skip_tag_newlines(s: &str, pos: usize) -> usize {
    let bytes = s.as_bytes();
    let mut p = pos;
    while p < bytes.len() && (bytes[p] == b'\n' || bytes[p] == b'\r') {
        p += 1;
    }
    p
}

/// Streaming parser that strips `<think>…</think>` blocks from assistant
/// `content` deltas, regardless of provider (MiniMax-M2, Qwen, etc. emit
/// reasoning inline this way). Inner text is returned as thoughts; everything
/// outside the tags is returned as normal text. Tag boundaries and their
/// adjacent newlines may be split arbitrarily across [`ThinkParser::feed`]
/// calls.
#[derive(Default)]
pub struct ThinkParser {
    in_think: bool,
    /// Set right after a tag is consumed with nothing following it yet, so
    /// newlines tag-adjacent but arriving in a later chunk are still dropped.
    strip_lead: bool,
    tail: String,
}

impl ThinkParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a `content` delta. Returns `(text, thoughts)` to emit; either may
    /// be empty. Bytes that could form a split tag are buffered internally.
    pub fn feed(&mut self, chunk: &str) -> (String, String) {
        self.tail.push_str(chunk);
        let mut text = String::new();
        let mut thoughts = String::new();

        // Drop newlines tag-adjacent to a tag consumed in a previous call.
        if self.strip_lead {
            let s = skip_tag_newlines(&self.tail, 0);
            self.tail.drain(..s);
            self.strip_lead = self.tail.is_empty();
        }

        loop {
            let (tag, out): (&str, &mut String) = if self.in_think {
                (THINK_CLOSE, &mut thoughts)
            } else {
                (THINK_OPEN, &mut text)
            };

            if let Some(p) = self.tail.find(tag) {
                out.push_str(&self.tail[..p]);
                let rest = skip_tag_newlines(&self.tail, p + tag.len());
                self.tail.drain(..rest);
                self.in_think = !self.in_think;
                self.strip_lead = self.tail.is_empty();
                continue;
            }

            let keep = partial_tag_suffix_len(&self.tail, tag);
            let flush_to = self.tail.len() - keep;
            out.push_str(&self.tail[..flush_to]);
            self.tail.drain(..flush_to);
            break;
        }

        (text, thoughts)
    }
}

/// Returns true when the SSE chunk's `delta` carries reasoning text in a
/// provider-specific dedicated field (e.g. DeepSeek's `reasoning_content`,
/// MiniMax's `thoughts`). Used to skip the inline `<think>…</think>`
/// pathway in the same delta so the same thought text isn't published
/// twice.
pub fn delta_has_reasoning_field(data: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(data) else {
        return false;
    };
    let Some(delta) = value
        .get("choices")
        .and_then(|choices| choices.get(0))
        .and_then(|choice| choice.get("delta"))
    else {
        return false;
    };
    ["reasoning_content", "thoughts"]
        .iter()
        .any(|key| delta.get(*key).and_then(|v| v.as_str()).is_some())
}

/// Process a single non-empty SSE data line (excluding `[DONE]` and comments).
///
/// Captures usage, accumulates tool-call partials, and returns any events that
/// should be emitted for this chunk (text deltas, completed tool calls on
/// `finish_reason`).  Also updates `last_usage` when a usage block is present.
pub fn process_sse_chunk(
    data: &str,
    partial_tool_calls: &mut BTreeMap<usize, PartialToolCall>,
    last_usage: &mut Option<Value>,
) -> Result<Vec<ChatEvent>, LlmError> {
    let value: Value = serde_json::from_str(data)?;
    let mut events = Vec::new();

    if let Some(usage) = value.get("usage") {
        *last_usage = Some(usage.clone());
    }

    let Some(choice) = value.get("choices").and_then(|choices| choices.get(0)) else {
        return Ok(events);
    };

    let Some(delta) = choice.get("delta") else {
        return Ok(events);
    };

    if let Some(content) = delta.get("content").and_then(|content| content.as_str())
        && !content.is_empty()
    {
        events.push(ChatEvent::TextDelta(content.to_string()));
    }

    // Providers carry reasoning in a provider-specific field (DeepSeek:
    // `reasoning_content`, MiniMax: `thoughts`). It must be echoed back under
    // that same field on later requests when tool calls are involved, so tag
    // the event with the wire key.
    for key in ["reasoning_content", "thoughts"] {
        if let Some(reasoning) = delta.get(key).and_then(|r| r.as_str()) {
            events.push(ChatEvent::ReasoningDelta {
                text: reasoning.to_string(),
                echo_field: Some(key.to_string()),
            });
        }
    }

    if let Some(tool_calls) = delta.get("tool_calls").and_then(|calls| calls.as_array()) {
        for (fallback_index, call) in tool_calls.iter().enumerate() {
            let index = call
                .get("index")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize)
                .unwrap_or(fallback_index);
            let partial = partial_tool_calls.entry(index).or_default();

            if let Some(id) = call.get("id").and_then(|v| v.as_str()) {
                partial.id.push_str(id);
            }

            let function = call.get("function").unwrap_or(&Value::Null);
            if let Some(name) = function.get("name").and_then(|v| v.as_str()) {
                partial.name.push_str(name);
            }
            if let Some(arguments) = function.get("arguments").and_then(|v| v.as_str()) {
                partial.arguments.push_str(arguments);
            }
        }
    }

    let finished_with_tool_calls = choice
        .get("finish_reason")
        .and_then(|reason| reason.as_str())
        .is_some_and(|reason| reason == "tool_calls" || reason == "function_call");

    if finished_with_tool_calls {
        events.extend(flush_partial_tool_calls(partial_tool_calls));
    }

    Ok(events)
}

#[async_trait]
impl LlmProvider for OpenAiCompatProvider {
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

    async fn validate(&self) -> Result<(), LlmError> {
        Ok(())
    }

    async fn chat_stream(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<ResponseStream, LlmError> {
        let stream_options = (self.base_url.contains("api.openai.com")
            || self.base_url.contains("127.0.0.1")
            || self.base_url.contains("localhost"))
        .then(|| StreamOptions {
            include_usage: true,
        });

        let request = ChatRequest {
            model: self.model.clone(),
            messages: openai_messages(messages),
            stream: true,
            tools: openai_tools(tools),
            stream_options,
            max_tokens: self.max_tokens,
        };

        let mut req = self.client.post(self.chat_url()).json(&request);

        if !self.api_key.is_empty() {
            req = req.bearer_auth(&self.api_key);
        }

        let response = req.send().await?;
        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            let details = error_body.trim();
            let message = if details.is_empty() {
                format!("HTTP {} from {}", status, self.chat_url())
            } else {
                format!("HTTP {} from {}: {}", status, self.chat_url(), details)
            };
            return Err(LlmError::new_with_kind(
                http_status_to_error_kind(status),
                message,
            ));
        }

        let events = response.bytes_stream().eventsource();

        let stream = try_stream! {
            futures_util::pin_mut!(events);
            let mut partial_tool_calls: BTreeMap<usize, PartialToolCall> = BTreeMap::new();
            let mut last_usage: Option<serde_json::Value> = None;
            let mut think = ThinkParser::new();

            while let Some(event) = events.try_next().await.map_err(|err| {
                LlmError::new_with_kind(LlmErrorKind::Connection, err.to_string())
            })? {
                let data = event.data.trim();
                if data.is_empty() {
                    continue;
                }

                if data == "[DONE]" {
                    // Flush partial tool calls — some providers send [DONE]
                    // without finish_reason: "tool_calls", which would
                    // silently drop tool calls and stall the agent loop.
                    for event in flush_partial_tool_calls(&mut partial_tool_calls) {
                        yield event;
                    }

                    if let Some(usage) = &last_usage {
                        yield ChatEvent::TokenUsage {
                            prompt_tokens: usage["prompt_tokens"].as_u64().unwrap_or(0) as u32,
                            completion_tokens: usage["completion_tokens"].as_u64().unwrap_or(0) as u32,
                            cached_tokens: usage
                                .get("prompt_tokens_details")
                                .and_then(|d| d.get("cached_tokens"))
                                .and_then(|v| v.as_u64())
                                .map(|v| v as u32),
                            cost: usage.get("cost").and_then(|v| v.as_f64()),
                        };
                    }
                    break;
                }

                if data.starts_with(':') {
                    continue;
                }

                // If the delta already carries reasoning via a dedicated
                // field (DeepSeek's `reasoning_content`, etc.), the inline
                // `<think>…</think>` pathway is not the source of truth
                // for this delta — skip it to avoid publishing the same
                // thought text twice.
                let reasoning_via_field = delta_has_reasoning_field(data);
                for event in process_sse_chunk(data, &mut partial_tool_calls, &mut last_usage)? {
                    match event {
                        ChatEvent::TextDelta(content) => {
                            let (text, thoughts) = if reasoning_via_field {
                                (content, String::new())
                            } else {
                                think.feed(&content)
                            };
                            if !text.is_empty() {
                                yield ChatEvent::TextDelta(text);
                            }
                            if !thoughts.is_empty() {
                                yield ChatEvent::ReasoningDelta {
                                    text: thoughts,
                                    echo_field: Some("thoughts".to_string()),
                                };
                            }
                        }
                        ChatEvent::ReasoningDelta { .. } => yield event,
                        other => yield other,
                    }
                }
            }

            // Flush any remaining partial tool calls on premature stream end.
            for event in flush_partial_tool_calls(&mut partial_tool_calls) {
                yield event;
            }
        };

        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::openai_messages;
    use crate::llm::{ChatMessage, ChatRole, ImageData};

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
}

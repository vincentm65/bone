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
    http_error, streaming_client,
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
    /// Optional transport overrides used by subscription-backed providers
    /// that speak the same Chat Completions wire format.
    api_key_override: Option<String>,
    extra_headers: Vec<(String, String)>,
    conversation_header: Option<(String, fn(i64) -> String)>,
}

impl OpenAiCompatProvider {
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
            api_key: entry.api_key.clone(),
            endpoint: entry.endpoint.clone(),
            max_tokens: None,
            api_key_override: None,
            extra_headers: Vec::new(),
            conversation_header: None,
        }
    }

    /// Build a Chat Completions provider for another authenticated endpoint
    /// while reusing the standard OpenAI-compatible message, tool, and stream
    /// handling.
    pub(crate) fn from_entry_with_transport(
        id: &str,
        entry: &ProviderEntry,
        api_key: String,
        extra_headers: Vec<(String, String)>,
        conversation_header: Option<(String, fn(i64) -> String)>,
    ) -> Self {
        let mut provider = Self::from_entry(id, entry);
        provider.api_key_override = Some(api_key);
        provider.extra_headers = extra_headers;
        provider.conversation_header = conversation_header;
        provider
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
        // A non-empty but unparseable argument string means the JSON was cut
        // off mid-stream (typically the output-token cap). Wrap the raw text in
        // a valid object keyed by `TRUNCATED_ARGS_KEY` rather than collapsing to
        // `Null` or keeping a bare string: the assistant message is persisted to
        // history and re-serialized on the next request, so it must stay valid
        // JSON (a bare string would be double-encoded into a malformed
        // `function.arguments`). The tool validator keys on the marker to report
        // truncation instead of an identical retry.
        let arguments = serde_json::from_str(&call.arguments).unwrap_or_else(|_| {
            Value::Object(serde_json::Map::from_iter([(
                crate::tools::TRUNCATED_ARGS_KEY.to_string(),
                Value::String(call.arguments),
            )]))
        });
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

/// Extract cache-hit tokens from a provider `usage` block. Providers disagree
/// on where they report it: OpenAI/GLM nest it under
/// `prompt_tokens_details.cached_tokens`, while DeepSeek exposes a top-level
/// `prompt_cache_hit_tokens`.
fn cached_tokens_from_usage(usage: &Value) -> Option<u32> {
    usage
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_u64())
        .or_else(|| {
            usage
                .get("prompt_cache_hit_tokens")
                .and_then(|v| v.as_u64())
        })
        .map(|v| v as u32)
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

    // Some providers emit stray content tokens inside the same chunk as a
    // tool-call delta; drop those. Content in tool-call-free chunks is real
    // prose and must be kept even if a tool call streamed earlier.
    let has_tool_call_delta = delta
        .get("tool_calls")
        .and_then(|calls| calls.as_array())
        .is_some_and(|calls| !calls.is_empty());

    if let Some(content) = delta.get("content").and_then(|content| content.as_str())
        && !content.is_empty()
        && !has_tool_call_delta
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
        self.chat_stream_with_context(messages, tools, Default::default())
            .await
    }

    async fn chat_stream_with_context(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
        context: crate::llm::provider::ProviderRequestContext,
    ) -> Result<ResponseStream, LlmError> {
        let stream_options = (self.base_url.contains("api.openai.com")
            || self.base_url.contains("api.deepseek.com")
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

        let api_key = self.api_key_override.as_deref().unwrap_or(&self.api_key);
        if !api_key.is_empty() {
            req = req.bearer_auth(api_key);
        }

        for (name, value) in &self.extra_headers {
            req = req.header(name, value);
        }
        if let (Some((header, encode)), Some(conversation_id)) =
            (self.conversation_header.as_ref(), context.conversation_id)
        {
            req = req.header(header, encode(conversation_id));
        }

        let response = req.send().await?;
        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            return Err(http_error(status, &self.chat_url(), &error_body));
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
                            cached_tokens: cached_tokens_from_usage(usage),
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
    use super::{
        ChatEvent, cached_tokens_from_usage, openai_messages, openai_tools, process_sse_chunk,
    };
    use crate::llm::{ChatMessage, ChatRole, ImageData};
    use std::collections::BTreeMap;

    #[test]
    fn reads_openai_style_nested_cached_tokens() {
        // OpenAI / GLM nest cache hits under prompt_tokens_details.
        let usage = serde_json::json!({
            "prompt_tokens": 100,
            "prompt_tokens_details": { "cached_tokens": 64 }
        });
        assert_eq!(cached_tokens_from_usage(&usage), Some(64));
    }

    #[test]
    fn reads_deepseek_style_top_level_cache_hit_tokens() {
        // DeepSeek reports cache hits as a top-level usage field.
        let usage = serde_json::json!({
            "prompt_tokens": 100,
            "prompt_cache_hit_tokens": 96,
            "prompt_cache_miss_tokens": 4
        });
        assert_eq!(cached_tokens_from_usage(&usage), Some(96));
    }

    #[test]
    fn no_cache_field_yields_none() {
        let usage = serde_json::json!({ "prompt_tokens": 100 });
        assert_eq!(cached_tokens_from_usage(&usage), None);
    }

    #[test]
    fn ignores_content_in_same_chunk_as_tool_call_delta() {
        let mut partial_tool_calls = BTreeMap::new();
        let mut usage = None;
        let first = r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"shell","arguments":"{\"command\":"}}]}}]}"#;
        let stray = r#"{"choices":[{"delta":{"content":"]","tool_calls":[{"index":0,"function":{"arguments":"\"echo hi\"}"}}]}}]}"#;
        let done = r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#;

        assert!(
            process_sse_chunk(first, &mut partial_tool_calls, &mut usage)
                .unwrap()
                .is_empty()
        );
        assert!(
            process_sse_chunk(stray, &mut partial_tool_calls, &mut usage)
                .unwrap()
                .is_empty()
        );
        let events = process_sse_chunk(done, &mut partial_tool_calls, &mut usage).unwrap();

        assert_eq!(events.len(), 1);
        match &events[0] {
            ChatEvent::ToolCall(call) => {
                assert_eq!(call.name, "shell");
                assert_eq!(call.arguments["command"], "echo hi");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn wraps_truncated_tool_arguments_in_valid_marker_object() {
        // The model's argument JSON is cut off at the output-token cap, so the
        // stream finishes with an unclosed object. It must surface as a *valid*
        // object keyed by `TRUNCATED_ARGS_KEY` (not `Null`, not a bare string):
        // the message is persisted and re-serialized, so it has to stay valid
        // JSON while still letting the tool validator report truncation.
        let mut partial_tool_calls = BTreeMap::new();
        let mut usage = None;
        let start = r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"edit_file","arguments":"{\"path\":\"world.rs\",\"content\":\"use std"}}]}}]}"#;
        let done = r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#;

        assert!(
            process_sse_chunk(start, &mut partial_tool_calls, &mut usage)
                .unwrap()
                .is_empty()
        );
        let events = process_sse_chunk(done, &mut partial_tool_calls, &mut usage).unwrap();

        assert_eq!(events.len(), 1);
        match &events[0] {
            ChatEvent::ToolCall(call) => {
                assert_eq!(call.name, "edit_file");
                let raw = call.arguments[crate::tools::TRUNCATED_ARGS_KEY]
                    .as_str()
                    .expect("truncated args wrapped under the marker key");
                assert!(raw.starts_with("{\"path\""));
                // Re-serializing the persisted assistant message must stay valid
                // JSON — never a double-encoded bare string.
                let serialized = call.arguments.to_string();
                assert!(serde_json::from_str::<serde_json::Value>(&serialized).is_ok());
                assert!(serialized.starts_with('{'));
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

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

    #[test]
    fn serializes_tools_with_required_function_envelope() {
        let tools = openai_tools(vec![crate::tools::ToolDefinition {
            name: "shell".into(),
            description: "Run a command".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"command": {"type": "string"}}
            }),
        }]);
        let json = serde_json::to_value(tools).unwrap();

        assert_eq!(json[0]["type"], "function");
        assert_eq!(json[0]["function"]["name"], "shell");
        assert!(json[0].get("name").is_none());
    }
}

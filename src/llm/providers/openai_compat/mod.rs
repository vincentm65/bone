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

/// Generic OpenAI-compatible provider.
///
/// Works with any server that implements the `/chat/completions` streaming
/// endpoint (OpenAI format): llama.cpp, OpenRouter, GLM, Gemini, Kimi,
/// DeepSeek, etc.
///
/// The `endpoint` field in `ProviderEntry` controls the path appended to
/// `base_url`.  Default: `/chat/completions`.  For local llama.cpp you
/// typically set `endpoint: /v1/chat/completions`.
pub struct OpenAiCompatProvider {
    client: reqwest::Client,
    base_url: String,
    model: String,
    api_key: String,
    endpoint: String,
    id: String,
    label: String,
}

impl OpenAiCompatProvider {
    pub fn from_entry(id: &str, entry: &ProviderEntry) -> Self {
        let label = if entry.label.is_empty() {
            id.to_string()
        } else {
            entry.label.clone()
        };
        Self {
            client: reqwest::Client::new(),
            id: id.to_string(),
            label,
            base_url: entry.base_url.trim_end_matches('/').to_string(),
            model: entry.model.clone(),
            api_key: entry.api_key.clone(),
            endpoint: entry.endpoint.clone(),
        }
    }

    fn chat_url(&self) -> String {
        format!("{}{}", self.base_url, self.endpoint)
    }

    fn is_local(&self) -> bool {
        self.base_url.contains("127.0.0.1") || self.base_url.contains("localhost")
    }
}

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<OpenAiMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<OpenAiTool>,
    /// Ask OpenAI-compat servers (OpenAI, llama.cpp, ollama, etc.) to include token
    /// usage in the final streaming chunk.  Ignored by servers that don't
    /// support it.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    stream_options: Option<StreamOptions>,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Serialize)]
struct OpenAiMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<OpenAiToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    /// DeepSeek V4 thinking mode requires this to be passed back when
    /// the assistant turn involved tool calls.
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_content: Option<String>,
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
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
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

fn openai_messages(messages: Vec<ChatMessage>) -> Vec<OpenAiMessage> {
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
            content: if message.content.is_empty() && !message.tool_calls.is_empty() {
                None
            } else {
                Some(message.content)
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
            reasoning_content: message.reasoning_content,
        })
        .collect()
}

/// Flush accumulated partial tool calls, emitting a [`ChatEvent::ToolCall`]
/// for each complete entry (id and name must be non-empty).
fn flush_partial_tool_calls(
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

/// Process a single non-empty SSE data line (excluding `[DONE]` and comments).
///
/// Captures usage, accumulates tool-call partials, and returns any events that
/// should be emitted for this chunk (text deltas, completed tool calls on
/// `finish_reason`).  Also updates `last_usage` when a usage block is present.
fn process_sse_chunk(
    data: &str,
    partial_tool_calls: &mut BTreeMap<usize, PartialToolCall>,
    last_usage: &mut Option<Value>,
) -> Result<Vec<ChatEvent>, LlmError> {
    let value: Value = serde_json::from_str(data)?;
    let mut events = Vec::new();

    // Capture usage data from any chunk
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

    // DeepSeek V4 thinking mode sends reasoning_content in the delta.
    // Must be captured and passed back in subsequent requests when tool
    // calls are involved, or DeepSeek returns 400.
    if let Some(reasoning) = delta
        .get("reasoning_content")
        .and_then(|r| r.as_str())
    {
        events.push(ChatEvent::ReasoningDelta(reasoning.to_string()));
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

    async fn validate(&self) -> Result<(), LlmError> {
        // Only attempt health check for local providers, as others like Gemini
        // might not have a /health endpoint or might require an API key.
        if self.is_local() && self.api_key.is_empty() {
            let health_url = format!("{}/health", self.base_url);
            let resp = self.client.get(&health_url).send().await;
            match resp {
                Ok(r) if r.status().is_success() => Ok(()),
                Ok(r) => Err(LlmError::new_with_kind(
                    http_status_to_error_kind(r.status()),
                    format!(
                        "local server returned {} from /health — is llama.cpp running?",
                        r.status()
                    ),
                )),
                Err(e) => Err(LlmError::new_with_kind(
                    LlmErrorKind::Connection,
                    format!(
                        "can't reach {}/health: {e} — is llama.cpp running?",
                        self.base_url
                    ),
                )),
            }
        } else {
            Ok(())
        }
    }

    async fn chat_stream(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<ResponseStream, LlmError> {
        let stream_options =
            (self.base_url.contains("api.openai.com") || self.is_local()).then(|| StreamOptions {
                include_usage: true,
            });

        let request = ChatRequest {
            model: self.model.clone(),
            messages: openai_messages(messages),
            stream: true,
            tools: openai_tools(tools),
            stream_options,
        };

        let mut req = self.client.post(self.chat_url()).json(&request);

        if !self.api_key.is_empty() {
            req = req.bearer_auth(&self.api_key);
        }

        let response = req.send().await?;
        let status = response.status();
        if !status.is_success() {
            return Err(LlmError::new_with_kind(
                http_status_to_error_kind(status),
                format!("HTTP {} from {}", status, self.chat_url()),
            ));
        }

        let events = response.bytes_stream().eventsource();

        let stream = try_stream! {
            futures_util::pin_mut!(events);
            let mut partial_tool_calls: BTreeMap<usize, PartialToolCall> = BTreeMap::new();
            let mut last_usage: Option<serde_json::Value> = None;

            while let Some(event) = events.try_next().await.map_err(|err| {
                LlmError::new_with_kind(LlmErrorKind::Connection, err.to_string())
            })? {
                let data = event.data.trim();
                if data.is_empty() {
                    continue;
                }

                if data == "[DONE]" {
                    // Flush any accumulated partial tool calls that haven't
                    // been emitted yet.  Some providers send [DONE] without
                    // a preceding finish_reason: "tool_calls" chunk, which
                    // would otherwise silently drop the tool calls and cause
                    // the agent loop to stop mid-task.
                    for event in flush_partial_tool_calls(&mut partial_tool_calls) {
                        yield event;
                    }

                    // Emit accumulated token usage if available
                    if let Some(usage) = &last_usage {
                        yield ChatEvent::TokenUsage {
                            prompt_tokens: usage["prompt_tokens"].as_u64().unwrap_or(0) as u32,
                            completion_tokens: usage["completion_tokens"].as_u64().unwrap_or(0) as u32,
                        };
                    }
                    break;
                }

                // Skip SSE comments (OpenRouter sends these)
                if data.starts_with(':') {
                    continue;
                }

                for event in process_sse_chunk(data, &mut partial_tool_calls, &mut last_usage)? {
                    yield event;
                }
            }

            // Safety net: if the stream ended without [DONE] (e.g.
            // connection drop), flush any remaining partial tool calls so
            // the agent loop doesn't silently stop.
            for event in flush_partial_tool_calls(&mut partial_tool_calls) {
                yield event;
            }
        };

        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests;

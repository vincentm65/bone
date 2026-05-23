use async_stream::try_stream;
use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

use std::path::Path;

use crate::config::ProviderEntry;
use crate::llm::provider::{
    ChatEvent, ChatMessage, ChatRole, LlmError, LlmErrorKind, LlmProvider, ResponseStream,
};
use crate::tools::{ToolCall, ToolDefinition};

/// Codex provider — adapts the ChatGPT Codex Responses API (`/responses` endpoint)
/// back into bone's OpenAI-compatible internal shape.
///
/// Key differences from OpenAI `/chat/completions`:
/// - Uses `instructions` + `input` instead of `messages`
/// - SSE events: `response.output_text.delta`, `response.completed`
/// - Tools use Codex function schema format
/// - Response normalization maps Codex output items into OpenAI-style messages
pub struct CodexProvider {
    client: reqwest::Client,
    base_url: String,
    model: String,
    api_key: String,
    endpoint: String,
    id: String,
    label: String,
}

/// Read access token from Codex CLI's cached auth (~/.codex/auth.json).
/// Returns the access_token if available, empty string otherwise.
fn read_codex_token() -> String {
    let path = Path::new(&dirs::home_dir().unwrap_or_default()).join(".codex/auth.json");
    let Ok(data) = std::fs::read_to_string(&path) else {
        return String::new();
    };
    let Ok(doc): Result<Value, _> = serde_json::from_str(&data) else {
        return String::new();
    };
    doc["tokens"]["access_token"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

/// Resolve the API key: prefer the dynamic Codex CLI token, fall back to config.
fn resolve_codex_api_key(config_key: &str) -> String {
    let token = read_codex_token();
    if !token.is_empty() {
        return token;
    }
    config_key.to_string()
}

impl CodexProvider {
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
}

// ── Request types ──────────────────────────────────────────────────────────

#[derive(Serialize)]
struct CodexRequest {
    model: String,
    instructions: String,
    input: Vec<CodexInputItem>,
    stream: bool,
    store: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<CodexTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_key: Option<String>,
}

#[derive(Serialize)]
struct CodexInputItem {
    #[serde(flatten)]
    fields: BTreeMap<String, Value>,
}

impl CodexInputItem {
    fn assistant_text(text: &str) -> Self {
        let mut fields = BTreeMap::new();
        fields.insert("role".to_string(), "assistant".into());
        fields.insert("content".into(), serde_json::json!([{"type": "output_text", "text": text}]));
        Self { fields }
    }

    fn user_text(text: &str) -> Self {
        let mut fields = BTreeMap::new();
        fields.insert("role".to_string(), "user".into());
        fields.insert("content".into(), serde_json::json!([{"type": "input_text", "text": text}]));
        Self { fields }
    }

    fn tool_call(call_id: &str, name: &str, arguments: &str) -> Self {
        let mut fields = BTreeMap::new();
        fields.insert("type".to_string(), "function_call".into());
        fields.insert("call_id".to_string(), Value::String(call_id.to_string()));
        fields.insert("name".to_string(), Value::String(name.to_string()));
        fields.insert("arguments".to_string(), Value::String(arguments.to_string()));
        Self { fields }
    }

    fn tool_result(call_id: &str, output: &str) -> Self {
        let mut fields = BTreeMap::new();
        fields.insert("type".to_string(), "function_call_output".into());
        fields.insert("call_id".to_string(), Value::String(call_id.to_string()));
        fields.insert("output".to_string(), Value::String(output.to_string()));
        Self { fields }
    }
}

#[derive(Serialize)]
struct CodexTool {
    r#type: &'static str,
    name: String,
    description: String,
    parameters: Value,
    strict: bool,
}

// ── Response types ─────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct CodexSSEEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    delta: Option<String>,
    #[serde(default)]
    response: Option<CodexResponse>,
}

#[derive(Deserialize)]
struct CodexOutputItem {
    #[serde(rename = "type")]
    item_type: String,
    #[serde(default)]
    call_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct CodexResponse {
    #[serde(default)]
    output: Vec<CodexOutputItem>,
    #[serde(default)]
    usage: Option<CodexUsage>,
}

#[derive(Deserialize)]
struct CodexUsage {
    #[serde(rename = "input_tokens")]
    input_tokens: Option<u64>,
    #[serde(rename = "output_tokens")]
    output_tokens: Option<u64>,
    #[serde(rename = "total_tokens")]
    total_tokens: Option<u64>,
}

// ── Provider implementation ────────────────────────────────────────────────

fn codex_tools(tools: Vec<ToolDefinition>) -> Vec<CodexTool> {
    tools
        .into_iter()
        .map(|tool| CodexTool {
            r#type: "function",
            name: tool.name.to_string(),
            description: tool.description.to_string(),
            parameters: tool.input_schema,
            strict: false,
        })
        .collect()
}

fn build_codex_messages(messages: Vec<ChatMessage>) -> Vec<CodexInputItem> {
    let mut items = Vec::new();
    for msg in messages {
        match msg.role {
            ChatRole::System => {
                // System messages become the instructions (handled separately)
                continue;
            }
            ChatRole::User => {
                items.push(CodexInputItem::user_text(&msg.content));
            }
            ChatRole::Assistant => {
                if !msg.content.is_empty() {
                    items.push(CodexInputItem::assistant_text(&msg.content));
                }
                for tool_call in msg.tool_calls {
                    let args_str = tool_call.arguments.to_string();
                    items.push(CodexInputItem::tool_call(
                        &tool_call.id,
                        &tool_call.name,
                        &args_str,
                    ));
                }
            }
            ChatRole::Tool => {
                items.push(CodexInputItem::tool_result(
                    msg.tool_call_id.as_deref().unwrap_or(""),
                    &msg.content,
                ));
            }
        }
    }
    items
}

fn build_instructions(messages: &[ChatMessage]) -> String {
    let system_parts: Vec<String> = messages
        .iter()
        .filter(|m| m.role == ChatRole::System)
        .map(|m| m.content.clone())
        .collect();
    if system_parts.is_empty() {
        "You are a helpful assistant.".to_string()
    } else {
        system_parts.join("\n")
    }
}

/// Extract tool calls and usage from the completed response.
///
/// Text is NOT emitted here — it was already streamed via
/// `response.output_text.delta` events.  Re-emitting it would duplicate
/// the assistant's content in the transcript and confuse the LLM on
/// subsequent rounds.
fn normalize_codex_response(resp: &CodexResponse) -> (Vec<ChatEvent>, Option<(u32, u32)>) {
    let mut events = Vec::new();
    let mut tool_calls: BTreeMap<usize, PartialToolCall> = BTreeMap::new();
    let mut usage: Option<(u32, u32)> = None;

    // Extract usage
    if let Some(usage_data) = &resp.usage {
        usage = usage_data
            .input_tokens
            .map(|i| i as u32)
            .zip(usage_data.output_tokens.map(|o| o as u32))
            .or_else(|| usage_data.total_tokens.map(|t| (t as u32 / 2, t as u32 - t as u32 / 2)));
    }

    for item in &resp.output {
        match item.item_type.as_str() {
            "function_call" => {
                let idx = tool_calls.len();
                tool_calls.insert(
                    idx,
                    PartialToolCall {
                        id: item.call_id.clone().unwrap_or_default(),
                        name: item.name.clone().unwrap_or_default(),
                        arguments: item.arguments.clone().unwrap_or_default(),
                    },
                );
            }
            _ => {}
        }
    }

    // Emit tool calls
    for (_, call) in tool_calls {
        if !call.id.is_empty() && !call.name.is_empty() {
            let args = serde_json::from_str(&call.arguments).unwrap_or(Value::Null);
            events.push(ChatEvent::ToolCall(ToolCall {
                id: call.id,
                name: call.name,
                arguments: args,
            }));
        }
    }

    (events, usage)
}

#[derive(Debug, Default)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
}

#[async_trait]
impl LlmProvider for CodexProvider {
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
        // Codex requires auth; a quick probe won't work without a real request.
        // Skip validation — errors will surface on first chat call.
        Ok(())
    }

    async fn chat_stream(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<ResponseStream, LlmError> {
        let instructions = build_instructions(&messages);
        let input_items = build_codex_messages(messages);
        let codex_tools = codex_tools(tools);

        let request = CodexRequest {
            model: self.model.clone(),
            instructions,
            input: input_items,
            stream: true,
            store: false,
            temperature: None,
            top_p: None,
            tools: if codex_tools.is_empty() {
                None
            } else {
                Some(codex_tools)
            },
            prompt_cache_key: None,
        };

        let mut req = self.client.post(self.chat_url()).json(&request);

        let api_key = resolve_codex_api_key(&self.api_key);
        if !api_key.is_empty() {
            req = req.bearer_auth(&api_key);
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
            let mut last_usage: Option<(u32, u32)> = None;

            while let Some(event) = events.try_next().await.map_err(|err| {
                LlmError::new_with_kind(LlmErrorKind::Connection, err.to_string())
            })? {
                let data = event.data.trim();
                if data.is_empty() {
                    continue;
                }

                if data == "[DONE]" {
                    break;
                }

                // Skip SSE comments
                if data.starts_with(':') {
                    continue;
                }

                let event: CodexSSEEvent = match serde_json::from_str(data) {
                    Ok(e) => e,
                    Err(_) => continue,
                };

                match event.event_type.as_str() {
                    "response.output_text.delta" => {
                        if let Some(delta) = event.delta {
                            yield ChatEvent::TextDelta(delta);
                        }
                    }
                    "response.completed" => {
                        if let Some(resp) = event.response {
                            let (events, usage) = normalize_codex_response(&resp);
                            if let Some(u) = usage {
                                last_usage = Some(u);
                            }
                            for event in events {
                                yield event;
                            }
                        }
                    }
                    "response.output_item.done" => {
                        // We collect output items in the response.completed event,
                        // but we can also process individual items here if needed.
                    }
                    _ => {}
                }
            }

            // Emit accumulated token usage
            if let Some((prompt, completion)) = last_usage {
                yield ChatEvent::TokenUsage {
                    prompt_tokens: prompt,
                    completion_tokens: completion,
                };
            }
        };

        Ok(Box::pin(stream))
    }
}

/// Map an HTTP status code to an [`LlmErrorKind`].
fn http_status_to_error_kind(status: reqwest::StatusCode) -> LlmErrorKind {
    match status.as_u16() {
        401 | 403 => LlmErrorKind::Auth,
        429 => LlmErrorKind::RateLimit,
        code if code >= 500 => LlmErrorKind::Server(code),
        _ => LlmErrorKind::Config,
    }
}

#[cfg(test)]
mod tests;

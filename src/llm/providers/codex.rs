use async_stream::try_stream;
use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

use crate::config::ProviderEntry;
use crate::llm::provider::{
    ChatEvent, ChatMessage, ChatRole, LlmError, LlmErrorKind, LlmProvider, ResponseStream,
    http_status_to_error_kind,
};
use crate::tools::{ToolCall, ToolDefinition};

/// Codex provider — adapts the Codex Responses API to bone's internal shape.
/// Uses `instructions`+`input` (not messages), Codex-format tools,
/// and normalizes streaming SSE events including function_call deltas.
pub struct CodexProvider {
    client: reqwest::Client,
    base_url: String,
    model: String,
    api_key: String,
    endpoint: String,
    id: String,
    label: String,
}

impl CodexProvider {
    pub fn from_entry(id: &str, entry: &ProviderEntry) -> Self {
        let label = if entry.label.is_empty() {
            id.to_string()
        } else {
            entry.label.clone()
        };
        Self {
            client: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(10))
                .timeout(std::time::Duration::from_secs(300))
                .build()
                .unwrap_or_default(),
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
}

/// Typed input items for the Codex Responses API. Uses `#[serde(untagged)]`;
/// message variants have `role`+`content`, function variants have `type`+fields.
#[derive(Serialize)]
#[serde(untagged)]
pub enum CodexInputItem {
    Message {
        role: &'static str,
        content: Vec<CodexContent>,
    },
    FunctionCall {
        #[serde(rename = "type")]
        kind: &'static str,
        call_id: String,
        name: String,
        arguments: String,
    },
    FunctionCallOutput {
        #[serde(rename = "type")]
        kind: &'static str,
        call_id: String,
        output: String,
    },
}

#[derive(Serialize)]
#[serde(tag = "type")]
pub enum CodexContent {
    #[serde(rename = "input_text")]
    InputText { text: String },
    #[serde(rename = "output_text")]
    OutputText { text: String },
}

impl CodexInputItem {
    fn user_text(text: &str) -> Self {
        Self::Message {
            role: "user",
            content: vec![CodexContent::InputText {
                text: text.to_string(),
            }],
        }
    }

    fn assistant_text(text: &str) -> Self {
        Self::Message {
            role: "assistant",
            content: vec![CodexContent::OutputText {
                text: text.to_string(),
            }],
        }
    }

    fn tool_call(call_id: &str, name: &str, arguments: &str) -> Self {
        Self::FunctionCall {
            kind: "function_call",
            call_id: call_id.to_string(),
            name: name.to_string(),
            arguments: arguments.to_string(),
        }
    }

    fn tool_result(call_id: &str, output: &str) -> Self {
        Self::FunctionCallOutput {
            kind: "function_call_output",
            call_id: call_id.to_string(),
            output: output.to_string(),
        }
    }
}

#[derive(Serialize)]
pub struct CodexTool {
    pub r#type: &'static str,
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub strict: bool,
}

/// Partial tool call accumulated from streaming Responses API deltas.
#[derive(Debug, Default)]
struct PartialCodexToolCall {
    call_id: String,
    name: String,
    arguments: String,
}

impl PartialCodexToolCall {
    fn is_ready(&self) -> bool {
        !self.call_id.is_empty() && !self.name.is_empty()
    }
}

/// Flush completed partial tool calls, emitting a [`ChatEvent::ToolCall`]
/// for each one that has both id and name set.
fn flush_partial_tool_calls(partial: &mut BTreeMap<usize, PartialCodexToolCall>) -> Vec<ChatEvent> {
    let completed = std::mem::take(partial);
    let mut events = Vec::new();
    for (_, call) in completed {
        if call.is_ready() {
            let arguments = serde_json::from_str(&call.arguments).unwrap_or(Value::Null);
            events.push(ChatEvent::ToolCall(ToolCall {
                id: call.call_id,
                name: call.name,
                arguments,
            }));
        }
    }
    events
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

pub fn codex_tools(tools: Vec<ToolDefinition>) -> Vec<CodexTool> {
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

pub fn build_codex_messages(messages: Vec<ChatMessage>) -> Vec<CodexInputItem> {
    let mut items = Vec::new();
    for msg in messages {
        match msg.role {
            ChatRole::System => continue,
            ChatRole::User => items.push(CodexInputItem::user_text(&msg.content)),
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

pub fn build_instructions(messages: &[ChatMessage]) -> String {
    let system_parts: Vec<&str> = messages
        .iter()
        .filter(|m| m.role == ChatRole::System)
        .map(|m| m.content.as_str())
        .collect();
    if system_parts.is_empty() {
        "You are a helpful assistant.".to_string()
    } else {
        system_parts.join("\n")
    }
}

/// Extract tool calls and usage from a completed response object.
/// Text is NOT emitted here — it was already streamed via
/// `response.output_text.delta` events, and re-emitting would
/// duplicate content and confuse the LLM on subsequent rounds.
fn extract_response_events(resp: &CodexResponse) -> (Vec<ChatEvent>, Option<(u32, u32)>) {
    let tool_calls: Vec<ChatEvent> = resp
        .output
        .iter()
        .filter(|item| item.item_type == "function_call")
        .filter_map(|item| {
            let id = item.call_id.clone()?;
            let name = item.name.clone()?;
            if id.is_empty() || name.is_empty() {
                return None;
            }
            let args = serde_json::from_str(item.arguments.as_deref().unwrap_or("null"))
                .unwrap_or(Value::Null);
            Some(ChatEvent::ToolCall(ToolCall {
                id,
                name,
                arguments: args,
            }))
        })
        .collect();

    let usage = resp.usage.as_ref().and_then(|u| {
        u.input_tokens
            .map(|i| i as u32)
            .zip(u.output_tokens.map(|o| o as u32))
            .or_else(|| {
                u.total_tokens
                    .map(|t| (t as u32 / 2, t as u32 - t as u32 / 2))
            })
    });

    (tool_calls, usage)
}

/// Resolve the event type. First checks the JSON `type` field (Responses API
/// convention), then falls back to the SSE `event:` line. Some backends only
/// set the event type in the SSE event line, not inside the JSON body.
fn resolve_event_type<'a>(raw: &'a Value, sse_event: &'a str) -> &'a str {
    if let Some(t) = raw.get("type").and_then(|t| t.as_str()) {
        return t;
    }
    sse_event
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

    fn set_model(&mut self, model: String) {
        self.model = model;
    }

    async fn validate(&self) -> Result<(), LlmError> {
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
            let mut partial_tool_calls: BTreeMap<usize, PartialCodexToolCall> = BTreeMap::new();
            let mut emitted_tool_call_ids: BTreeSet<String> = BTreeSet::new();
            let mut last_usage: Option<(u32, u32)> = None;

            while let Some(event) = events.try_next().await.map_err(|err| {
                LlmError::new_with_kind(LlmErrorKind::Connection, err.to_string())
            })? {
                let data = event.data.trim();
                if data.is_empty() {
                    continue;
                }

                if data == "[DONE]" {
                    // Flush any partial tool calls that haven't been completed yet.
                    for ev in flush_partial_tool_calls(&mut partial_tool_calls) {
                        yield ev;
                    }
                    break;
                }

                if data.starts_with(':') {
                    continue;
                }

                let raw: Value = match serde_json::from_str(data) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                let event_type = resolve_event_type(&raw, &event.event);

                match event_type {
                    // ── Text streaming ──────────────────────────────────
                    "response.output_text.delta" => {
                        if let Some(delta) = raw.get("delta").and_then(|d| d.as_str()) {
                            yield ChatEvent::TextDelta(delta.to_string());
                        }
                    }

                    // ── Tool call streaming (output item lifecycle) ────
                    "response.output_item.added" => {
                        let output_index = raw
                            .get("output_index")
                            .and_then(|v| v.as_u64())
                            .map(|v| v as usize)
                            .unwrap_or(0);
                        if let Some(item) = raw.get("item")
                            && item.get("type").and_then(|t| t.as_str()) == Some("function_call") {
                                let mut partial = PartialCodexToolCall::default();
                                if let Some(id) = item.get("call_id").and_then(|v| v.as_str()) {
                                    partial.call_id = id.to_string();
                                }
                                if let Some(name) = item.get("name").and_then(|v| v.as_str()) {
                                    partial.name = name.to_string();
                                }
                                partial_tool_calls.insert(output_index, partial);
                            }
                    }

                    "response.function_call_arguments.delta" => {
                        let output_index = raw
                            .get("output_index")
                            .and_then(|v| v.as_u64())
                            .map(|v| v as usize)
                            .unwrap_or(0);
                        if let Some(delta) = raw.get("delta").and_then(|d| d.as_str()) {
                            partial_tool_calls
                                .entry(output_index)
                                .or_default()
                                .arguments
                                .push_str(delta);
                        }
                    }

                    "response.function_call_arguments.done" => {
                        let output_index = raw
                            .get("output_index")
                            .and_then(|v| v.as_u64())
                            .map(|v| v as usize)
                            .unwrap_or(0);
                        // Some backends send the full arguments string in this event.
                        if let Some(args) = raw.get("arguments").and_then(|a| a.as_str()) {
                            let partial = partial_tool_calls
                                .entry(output_index)
                                .or_default();
                            // Prefer the full string if provided; otherwise keep deltas.
                            if !args.is_empty() {
                                partial.arguments = args.to_string();
                            }
                        }
                    }

                    "response.output_item.done" => {
                        let output_index = raw
                            .get("output_index")
                            .and_then(|v| v.as_u64())
                            .map(|v| v as usize)
                            .unwrap_or(0);
                        if let Some(partial) = partial_tool_calls.remove(&output_index)
                            && partial.is_ready() {
                                let arguments =
                                    serde_json::from_str(&partial.arguments)
                                        .unwrap_or(Value::Null);
                                emitted_tool_call_ids.insert(partial.call_id.clone());
                                yield ChatEvent::ToolCall(ToolCall {
                                    id: partial.call_id,
                                    name: partial.name,
                                    arguments,
                                });
                            }
                    }

                    // ── Final response (fallback for non-streaming tool calls) ─
                    "response.completed" => {
                        // Flush any remaining partial calls first.
                        for ev in flush_partial_tool_calls(&mut partial_tool_calls) {
                            if let ChatEvent::ToolCall(call) = &ev {
                                emitted_tool_call_ids.insert(call.id.clone());
                            }
                            yield ev;
                        }
                        if let Some(resp_val) = raw.get("response")
                            && let Ok(resp) = serde_json::from_value(resp_val.clone()) {
                                let (events, usage) = extract_response_events(&resp);
                                if let Some(u) = usage {
                                    last_usage = Some(u);
                                }
                                for ev in events {
                                    match &ev {
                                        ChatEvent::ToolCall(call) if emitted_tool_call_ids.contains(&call.id) => {}
                                        ChatEvent::ToolCall(call) => {
                                            emitted_tool_call_ids.insert(call.id.clone());
                                            yield ev;
                                        }
                                        _ => yield ev,
                                    }
                                }
                            }
                    }

                    _ => {}
                }
            }

            // Flush any remaining partial tool calls on premature stream end.
            for ev in flush_partial_tool_calls(&mut partial_tool_calls) {
                yield ev;
            }

            if let Some((prompt, completion)) = last_usage {
                yield ChatEvent::TokenUsage {
                    prompt_tokens: prompt,
                    completion_tokens: completion,
                    cached_tokens: None,
                    cost: None,
                };
            }
        };

        Ok(Box::pin(stream))
    }
}

fn read_codex_token() -> String {
    let path = std::path::Path::new(&dirs::home_dir().unwrap_or_default()).join(".codex/auth.json");
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

fn resolve_codex_api_key(config_key: &str) -> String {
    let token = read_codex_token();
    if !token.is_empty() {
        return token;
    }
    config_key.to_string()
}

use async_stream::try_stream;
use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::ProviderEntry;
use crate::llm::provider::{
    ChatEvent, ChatMessage, ChatRole, LlmError, LlmErrorKind, LlmProvider, ResponseStream,
    http_status_to_error_kind,
};
use crate::tools::{ToolCall, ToolDefinition};

/// Codex provider — adapts the Codex Responses API to bone's internal shape.
/// Uses `instructions`+`input` (not messages), Codex-format tools,
/// and normalizes `response.output_text.delta` SSE events.
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

// ── Request types ──

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

// ── Response types ──

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

// ── Provider implementation ──

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

// Text is NOT emitted here — it was already streamed via
// `response.output_text.delta` events, and re-emitting would
// duplicate content and confuse the LLM on subsequent rounds.
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
                            let (events, usage) = extract_response_events(&resp);
                            if let Some(u) = usage {
                                last_usage = Some(u);
                            }
                            for event in events {
                                yield event;
                            }
                        }
                    }
                    _ => {}
                }
            }

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



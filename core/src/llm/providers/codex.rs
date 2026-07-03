//! Codex (OpenAI Responses API) provider implementation.

use async_stream::try_stream;
use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::config::ProviderEntry;
use crate::llm::provider::{
    ChatEvent, ChatMessage, ChatRole, LlmError, LlmErrorKind, LlmProvider, ProviderRequestContext,
    ResponseStream, http_error, streaming_client,
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
            client: streaming_client(),
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
pub struct CodexRequest {
    pub model: String,
    pub instructions: String,
    pub input: Vec<CodexInputItem>,
    pub stream: bool,
    pub store: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<CodexTool>>,
    /// Mirror the Codex CLI request shape: when tools are present it sends an
    /// explicit `tool_choice: "auto"`. This is the Responses API default, so it
    /// does not change selection behavior — it keeps the serialized request
    /// prefix byte-identical to the Codex CLI so cached prefixes line up.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<&'static str>,
    /// Codex-only stable cache key. Set per conversation/thread so same-thread
    /// requests route to the same cached-prefix backend, matching Codex CLI.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<String>,
    /// Request encrypted reasoning content so it can be replayed on the next
    /// turn when `store: false`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include: Option<Vec<&'static str>>,
}

/// Typed input items for the Codex Responses API. Uses `#[serde(untagged)]`;
/// message variants have `role`+`content`, function variants have `type`+fields.
#[derive(Serialize, Clone)]
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
    Reasoning {
        #[serde(rename = "type")]
        kind: &'static str,
        id: String,
        summary: Vec<String>,
        encrypted_content: String,
    },
}

#[derive(Serialize, Clone)]
#[serde(tag = "type")]
pub enum CodexContent {
    #[serde(rename = "input_text")]
    InputText { text: String },
    #[serde(rename = "input_image")]
    InputImage { image_url: String },
    #[serde(rename = "output_text")]
    OutputText { text: String },
}

impl CodexInputItem {
    fn user_message(text: &str, images: Vec<crate::llm::ImageData>) -> Self {
        let mut content = Vec::new();
        if !text.is_empty() {
            content.push(CodexContent::InputText {
                text: text.to_string(),
            });
        }
        for image in images {
            content.push(CodexContent::InputImage {
                image_url: format!("data:{};base64,{}", image.media_type, image.data),
            });
        }
        Self::Message {
            role: "user",
            content,
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
    fn reasoning(id: &str, encrypted_content: &str) -> Self {
        Self::Reasoning {
            kind: "reasoning",
            id: id.to_string(),
            summary: Vec::new(),
            encrypted_content: encrypted_content.to_string(),
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

use super::openai_compat::{PartialToolCall, flush_partial_tool_calls};

#[derive(Deserialize)]
struct CodexOutputItem {
    #[serde(rename = "type")]
    item_type: String,
    #[serde(default)]
    call_id: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
    #[serde(default)]
    encrypted_content: Option<String>,
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
    #[serde(default)]
    input_tokens_details: Option<CodexInputTokenDetails>,
}

pub fn codex_tools(tools: Vec<ToolDefinition>) -> Vec<CodexTool> {
    let mut tools = tools;
    tools.sort_by(|a, b| a.name.cmp(&b.name));
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
            ChatRole::User => items.push(CodexInputItem::user_message(&msg.content, msg.images)),
            ChatRole::Assistant if !msg.output_sequence.is_empty() => {
                // Replay the turn's output items in their original emission
                // order. Splitting reasoning and function calls into separate
                // runs (all reasoning, then all calls) breaks Responses
                // `store: false` validation and prefix-cache alignment when the
                // backend interleaved them (reasoning A, call A, reasoning B…).
                for item in &msg.output_sequence {
                    match item {
                        crate::llm::OutputItem::Reasoning(ri) => {
                            items.push(CodexInputItem::reasoning(&ri.id, &ri.encrypted_content));
                        }
                        crate::llm::OutputItem::Text(text) if !text.is_empty() => {
                            items.push(CodexInputItem::assistant_text(text));
                        }
                        crate::llm::OutputItem::Text(_) => {}
                        crate::llm::OutputItem::ToolCall(tc) => {
                            let args_str = tc.arguments.to_string();
                            items.push(CodexInputItem::tool_call(&tc.id, &tc.name, &args_str));
                        }
                    }
                }
            }
            ChatRole::Assistant => {
                // Fallback for messages with no recorded sequence (restored from
                // the session DB, which does not persist reasoning items).
                for ri in &msg.reasoning_items {
                    items.push(CodexInputItem::reasoning(&ri.id, &ri.encrypted_content));
                }
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

#[derive(Deserialize)]
struct CodexInputTokenDetails {
    #[serde(default)]
    cached_tokens: Option<u64>,
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
#[allow(clippy::type_complexity)]
fn extract_response_events(
    resp: &CodexResponse,
) -> (Vec<ChatEvent>, Option<(u32, u32, Option<u32>)>) {
    // Single pass over the output array so reasoning and function_call items
    // stay in the backend's original order. Splitting them into separate lists
    // (all reasoning, then all calls) would reorder interleaved sequences like
    // [reasoning A, call A, reasoning B, call B] and break `store: false`
    // validation / prefix-cache alignment.
    let mut events: Vec<ChatEvent> = Vec::new();
    for item in &resp.output {
        match item.item_type.as_str() {
            "function_call" => {
                let (Some(id), Some(name)) = (item.call_id.clone(), item.name.clone()) else {
                    continue;
                };
                if id.is_empty() || name.is_empty() {
                    continue;
                }
                let args = serde_json::from_str(item.arguments.as_deref().unwrap_or("null"))
                    .unwrap_or(Value::Null);
                events.push(ChatEvent::ToolCall(ToolCall {
                    id,
                    name,
                    arguments: args,
                }));
            }
            "reasoning" => {
                let (Some(id), Some(encrypted_content)) =
                    (item.id.clone(), item.encrypted_content.clone())
                else {
                    continue;
                };
                if id.is_empty() || encrypted_content.is_empty() {
                    continue;
                }
                events.push(ChatEvent::EncryptedReasoning {
                    id,
                    encrypted_content,
                });
            }
            _ => {}
        }
    }

    let usage = resp.usage.as_ref().and_then(|u| {
        let prompt = u.input_tokens.map(|i| i as u32);
        let completion = u.output_tokens.map(|o| o as u32);
        let cached = u
            .input_tokens_details
            .as_ref()
            .and_then(|d| d.cached_tokens)
            .map(|c| c as u32);
        prompt
            .zip(completion)
            .map(|(p, c)| (p, c, cached))
            .or_else(|| {
                u.total_tokens
                    .map(|t| (t as u32 / 2, t as u32 - t as u32 / 2, cached))
            })
    });

    (events, usage)
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

fn prompt_cache_key(context: &ProviderRequestContext) -> Option<String> {
    context.conversation_id.map(codex_session_id)
}

/// Stable per-conversation session/thread id for the Codex routing headers, in
/// UUIDv4 shape. The chatgpt backend proxy routes on a hash of this value, so
/// only stability per conversation matters; deriving it from the DB
/// conversation id also keeps it stable across process restarts, so a resumed
/// conversation stays pinned to the same (warm) cache shard.
fn codex_session_id(conversation_id: i64) -> String {
    let id = conversation_id as u64;
    format!("00000000-0000-4000-8000-{id:012x}")
}

/// Diagnostic gate: when `BONE_CODEX_DEBUG` is set (and not empty/`0`), each
/// Codex request body is dumped to its own file and its reported cache stats are
/// logged, so prefix-cache divergence can be located by diffing two consecutive
/// request JSONs. Zero-cost (a single env read) when unset.
fn codex_debug_enabled() -> bool {
    std::env::var("BONE_CODEX_DEBUG")
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false)
}

/// Monotonic request counter so a request dump and its later usage line share a
/// stable `#N` and consecutive requests are diffable in order.
static CODEX_DEBUG_SEQ: AtomicU64 = AtomicU64::new(0);

/// Append one line to `bone.log` (same file the Lua `ctx.log` helpers use).
fn codex_debug_log_line(line: &str) {
    let path = crate::config::bone_dir().join("bone.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write;
        let _ = writeln!(f, "{line}");
    }
}

/// Write the pretty-printed request body to `codex-debug-NNNN.json` and log a
/// one-line summary to `bone.log`. Returns the request's sequence number so the
/// matching usage line can reference it. Pretty-printed (one input item per
/// block) so a plain `diff` of two dumps points straight at the first item where
/// the cached prefix breaks.
fn codex_debug_dump_request(request: &CodexRequest) -> u64 {
    let seq = CODEX_DEBUG_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = crate::config::bone_dir();
    let file = dir.join(format!("codex-debug-{seq:04}.json"));
    let body = serde_json::to_string_pretty(request).unwrap_or_default();
    let _ = std::fs::write(&file, &body);
    codex_debug_log_line(&format!(
        "[codex-debug] req #{seq} model={} input_items={} cache_key={} -> {}",
        request.model,
        request.input.len(),
        request.prompt_cache_key.as_deref().unwrap_or("(none)"),
        file.display(),
    ));
    seq
}

/// Log the provider-reported usage for request `#seq`, including the cache-hit
/// rate (cached as a fraction of input), so the per-request hit rate can be
/// read straight from `bone.log` without aggregation.
fn codex_debug_log_usage(seq: u64, prompt: u32, completion: u32, cached: Option<u32>) {
    let cached = cached.unwrap_or(0);
    let pct = if prompt > 0 {
        (cached as f64 / prompt as f64) * 100.0
    } else {
        0.0
    };
    codex_debug_log_line(&format!(
        "[codex-debug] req #{seq} usage: input={prompt} cached={cached} ({pct:.1}%) output={completion}"
    ));
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
        self.chat_stream_with_context(messages, tools, ProviderRequestContext::default())
            .await
    }

    async fn chat_stream_with_context(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
        context: ProviderRequestContext,
    ) -> Result<ResponseStream, LlmError> {
        let instructions = build_instructions(&messages);
        let input_items = build_codex_messages(messages);
        let prompt_cache_key = prompt_cache_key(&context);
        let codex_tools = codex_tools(tools);
        let tools = if codex_tools.is_empty() {
            None
        } else {
            Some(codex_tools)
        };
        // Codex CLI only emits `tool_choice` when it actually sends tools.
        let tool_choice = tools.as_ref().map(|_| "auto");

        let request = CodexRequest {
            model: self.model.clone(),
            instructions,
            input: input_items,
            stream: true,
            store: false,
            temperature: None,
            top_p: None,
            tools,
            tool_choice,
            prompt_cache_key,
            include: Some(vec!["reasoning.encrypted_content"]),
        };

        // Diagnostic: dump the request body (gated by BONE_CODEX_DEBUG) so the
        // matching usage line can correlate by sequence number.
        let debug_seq = codex_debug_enabled().then(|| codex_debug_dump_request(&request));

        let mut req = self.client.post(self.chat_url()).json(&request);

        let api_key = resolve_codex_api_key(&self.api_key);
        if !api_key.is_empty() {
            req = req.bearer_auth(&api_key);
        }

        // Mirror the Codex CLI request identity so the chatgpt backend proxy
        // pins a conversation's turns to one cache shard. The body's
        // `prompt_cache_key` alone is not enough: the proxy load-balances each
        // turn onto a different shard unless these routing headers are present,
        // so the growing conversation prefix gets re-billed at the fresh-input
        // rate on most turns (the oscillating per-turn cache miss we measured).
        req = req.header("originator", "codex_cli_rs");
        if let Some(conv_id) = context.conversation_id {
            let session_id = codex_session_id(conv_id);
            req = req
                .header("session-id", &session_id)
                .header("thread-id", &session_id)
                .header("x-client-request-id", &session_id);
        }
        if let Some(turn_state) = context.turn_state.as_ref().and_then(|state| state.get()) {
            req = req.header("x-codex-turn-state", turn_state);
        }

        let response = req.send().await?;
        let status = response.status();
        if !status.is_success() {
            let url = self.chat_url();
            let body = response.text().await.unwrap_or_default();
            return Err(http_error(status, &url, &body));
        }

        // Capture before consuming the response body. OnceLock deliberately
        // keeps the first value if a retry or later tool round returns another.
        if let (Some(turn_state), Some(value)) = (
            context.turn_state.as_ref(),
            response.headers().get("x-codex-turn-state"),
        ) {
            if let Ok(value) = value.to_str() {
                let _ = turn_state.set(value.to_owned());
            }
        }

        let events = response.bytes_stream().eventsource();

        let stream = try_stream! {
            futures_util::pin_mut!(events);
            let mut partial_tool_calls: BTreeMap<usize, PartialToolCall> = BTreeMap::new();
            let mut emitted_tool_call_ids: BTreeSet<String> = BTreeSet::new();
            let mut emitted_reasoning_ids: BTreeSet<String> = BTreeSet::new();
            let mut last_usage: Option<(u32, u32, Option<u32>)> = None;

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
                                let mut partial = PartialToolCall::default();
                                if let Some(id) = item.get("call_id").and_then(|v| v.as_str()) {
                                    partial.id = id.to_string();
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
                        // Encrypted reasoning items: emit for in-memory replay.
                        if let Some(item) = raw.get("item")
                            && item.get("type").and_then(|t| t.as_str()) == Some("reasoning")
                            && let Some(id) = item.get("id").and_then(|v| v.as_str())
                            && let Some(enc) = item.get("encrypted_content").and_then(|v| v.as_str())
                            && !id.is_empty() && !enc.is_empty() {
                                emitted_reasoning_ids.insert(id.to_string());
                                yield ChatEvent::EncryptedReasoning {
                                    id: id.to_string(),
                                    encrypted_content: enc.to_string(),
                                };
                            }
                        if let Some(partial) = partial_tool_calls.remove(&output_index)
                            && !partial.id.is_empty() && !partial.name.is_empty() {
                                let arguments =
                                    serde_json::from_str(&partial.arguments)
                                        .unwrap_or(Value::Null);
                                emitted_tool_call_ids.insert(partial.id.clone());
                                yield ChatEvent::ToolCall(ToolCall {
                                    id: partial.id,
                                    name: partial.name,
                                    arguments,
                                });
                            }
                    }

                    // ── Terminal failure events ─────────────────────────
                    // A response can end with a server-side failure instead
                    // of `response.completed`. Surface it as a stream error
                    // instead of silently ending the turn with an empty
                    // assistant message.
                    "response.failed" | "error" => {
                        let msg = raw
                            .pointer("/response/error/message")
                            .or_else(|| raw.pointer("/error/message"))
                            .or_else(|| raw.get("message"))
                            .and_then(|v| v.as_str())
                            .unwrap_or(data);
                        Err(LlmError::new_with_kind(
                            LlmErrorKind::Connection,
                            format!("codex response failed: {msg}"),
                        ))?;
                    }

                    "response.incomplete" => {
                        let reason = raw
                            .pointer("/response/incomplete_details/reason")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown reason");
                        Err(LlmError::new_with_kind(
                            LlmErrorKind::Config,
                            format!("codex response incomplete: {reason}"),
                        ))?;
                    }

                    "response.completed" => {
                        // Flush any remaining partial calls first.
                        for ev in flush_partial_tool_calls(&mut partial_tool_calls) {
                            if let ChatEvent::ToolCall(call) = &ev {
                                emitted_tool_call_ids.insert(call.id.clone());
                            }
                            yield ev;
                        }
                        if let Some(resp_val) = raw.get("response")
                            && let Ok(resp) = serde_json::from_value::<CodexResponse>(resp_val.clone()) {
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
                                        ChatEvent::EncryptedReasoning { id, .. } if emitted_reasoning_ids.contains(id) => {}
                                        ChatEvent::EncryptedReasoning { id, .. } => {
                                            emitted_reasoning_ids.insert(id.clone());
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

            if let Some((prompt, completion, cached)) = last_usage {
                if let Some(seq) = debug_seq {
                    codex_debug_log_usage(seq, prompt, completion, cached);
                }
                yield ChatEvent::TokenUsage {
                    prompt_tokens: prompt,
                    completion_tokens: completion,
                    cached_tokens: cached,
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

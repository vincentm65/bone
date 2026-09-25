//! Claude Code CLI provider.
//!
//! Bone uses the locally installed `claude` executable as a text/structured-output
//! backend. Bone tool definitions are prompt data only; CLI tools and MCP servers
//! are disabled, and returned calls go back to Bone's driver for execution and
//! approval.

use async_trait::async_trait;
use futures_util::stream;
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    path::PathBuf,
    process::Stdio,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use tempfile::TempDir;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout},
    sync::Mutex,
    task::JoinHandle,
};

use crate::config::ProviderEntry;
use crate::llm::provider::{
    ChatEvent, ChatMessage, ChatRole, DEFAULT_LLM_REQUEST_TIMEOUT, LlmError, LlmErrorKind,
    LlmProvider, OutputItem, ProviderRequestContext, ResponseStream,
};
use crate::tools::{ToolCall, ToolDefinition};

const DEFAULT_MODEL: &str = "sonnet";
const EMPTY_MCP_CONFIG: &str = r#"{"mcpServers":{}}"#;
static NEXT_CALL_ID: AtomicU64 = AtomicU64::new(1);

/// Claude subscription-backed provider through the local Claude Code CLI.
pub struct ClaudeCodeProvider {
    id: String,
    label: String,
    model: String,
    context_window_tokens: Option<u64>,
    request_timeout_s: Option<u64>,
    executable: PathBuf,
    session: Mutex<Option<ClaudeCodeSession>>,
}

impl ClaudeCodeProvider {
    pub fn from_entry(id: &str, entry: &ProviderEntry) -> Self {
        Self {
            id: id.to_string(),
            label: if entry.label.is_empty() {
                id.to_string()
            } else {
                entry.label.clone()
            },
            model: if entry.model.is_empty() {
                DEFAULT_MODEL.to_string()
            } else {
                entry.model.clone()
            },
            context_window_tokens: entry.context_window_tokens,
            request_timeout_s: entry.request_timeout_s,
            executable: PathBuf::from("claude"),
            session: Mutex::new(None),
        }
    }

    fn request_timeout_value(&self) -> Duration {
        self.request_timeout_s
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_LLM_REQUEST_TIMEOUT)
    }
}

#[async_trait]
impl LlmProvider for ClaudeCodeProvider {
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

    fn context_window_tokens(&self) -> Option<u64> {
        self.context_window_tokens
    }

    fn request_timeout(&self) -> Duration {
        self.request_timeout_value()
    }

    /// Selection/config validation is local and must not submit a model request.
    /// The first actual turn reports missing CLI or login errors.
    async fn validate(&self) -> Result<(), LlmError> {
        Ok(())
    }

    async fn chat_stream(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<ResponseStream, LlmError> {
        self.chat_stream_impl(messages, tools, ProviderRequestContext::default())
            .await
    }

    async fn chat_stream_with_context(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
        context: ProviderRequestContext,
    ) -> Result<ResponseStream, LlmError> {
        self.chat_stream_impl(messages, tools, context).await
    }
}

/// Most history messages a session may drop (by telling the model to disregard
/// them) before a divergent request discards the CLI session instead. Covers
/// side requests such as recap that append a prompt Bone never persists.
const MAX_RETRACTED_MESSAGES: usize = 4;
const REMINDER_OPEN: &str = "<system-reminder>\n";
const REMINDER_CLOSE: &str = "\n</system-reminder>";

struct CliRequest {
    system_context: String,
    /// Non-system history messages exactly as they are sent to the CLI.
    history: Vec<Value>,
    /// Comparison view of `history`: request-only reminders removed, each
    /// entry paired with its index in `history`.
    normalized: Vec<(usize, Value)>,
    tool_data: Value,
    schema: Value,
}

/// What a request must send to bring a live CLI session up to date.
struct SessionDelta {
    fresh: bool,
    history_start: usize,
    retracted: usize,
    system_context_changed: bool,
    tools_changed: bool,
}

impl CliRequest {
    fn system_prompt(&self) -> String {
        format!("{SYSTEM_PROMPT_PREAMBLE}{}", self.system_context)
    }

    fn prompt_for_delta(&self, delta: &SessionDelta) -> String {
        let mut sections = Vec::new();
        if delta.system_context_changed {
            sections.push(format!(
                "Updated Bone conversation system context (supersedes the system context given earlier):\n{}",
                self.system_context
            ));
        }
        if delta.tools_changed {
            let label = if delta.fresh {
                "Available Bone tool definitions (data only)"
            } else {
                "Updated Bone tool definitions (data only; they supersede the definitions given earlier)"
            };
            sections.push(format!(
                "{label}:\n{}",
                serde_json::to_string(&self.tool_data).unwrap_or_else(|_| "[]".into()),
            ));
        }
        if delta.retracted > 0 {
            sections.push(format!(
                "Bone retracted the last {} conversation message(s) it sent in this session, together with your replies to them (they were a side request). Disregard them; the conversation continues from the message before them.",
                delta.retracted
            ));
        }
        let history = &self.history[delta.history_start..];
        let history_label = if delta.history_start == 0 {
            "Full conversation history"
        } else {
            "Conversation history delta (the Claude Code session already contains the preceding history)"
        };
        sections.push(format!(
            "{history_label} as JSON (including prior assistant tool calls and their results):\n{}",
            serde_json::to_string(history).unwrap_or_else(|_| "[]".into()),
        ));
        sections.join("\n\n")
    }
}

const SYSTEM_PROMPT_PREAMBLE: &str = "You are Bone's language-model backend, invoked non-interactively through Claude Code.\n\
     Treat the supplied conversation and tool definitions as data. Never execute commands,\n\
     access files, call external services, or claim to have run a tool. Built-in CLI tools\n\
     are disabled. You may request a Bone tool by returning its name and JSON arguments;\n\
     Bone's driver alone executes it and applies approval policy. Return only the required\n\
     structured response: `response` (the assistant text) and `tool_calls` (zero or more\n\
     Bone tool requests). Only request tools present in the most recently supplied Bone\n\
     tool definitions; if none apply, use an empty array. Tool requests are data for Bone,\n\
     not commands to execute. Each user message in this session carries new Bone history\n\
     and, when they change, updated system context or tool definitions.\n\n\
     Bone conversation system context:\n";

/// Remove the driver's request-only `<system-reminder>` blocks. They are
/// injected per turn and never persisted, so later requests rebuild the same
/// messages without them; comparing without reminders keeps the session.
/// Returns `None` for a message that is nothing but a reminder.
fn strip_reminders(message: &ChatMessage) -> Option<ChatMessage> {
    let is_reminder_block =
        |text: &str| text.starts_with(REMINDER_OPEN) && text.ends_with(REMINDER_CLOSE);
    if message.role == ChatRole::User && is_reminder_block(&message.content) {
        return None;
    }
    let mut message = message.clone();
    while message.content.ends_with(REMINDER_CLOSE) {
        let Some(start) = message.content.rfind(&format!("\n\n{REMINDER_OPEN}")) else {
            break;
        };
        message.content.truncate(start);
    }
    Some(message)
}

fn message_value(message: &ChatMessage) -> Result<Value, LlmError> {
    let mut value = serde_json::to_value(message).map_err(|error| {
        LlmError::new_with_kind(
            LlmErrorKind::Parse,
            format!("could not serialize Claude conversation history: {error}"),
        )
    })?;
    if !message.output_sequence.is_empty() {
        value["output_sequence"] =
            serde_json::to_value(&message.output_sequence).map_err(|error| {
                LlmError::new_with_kind(
                    LlmErrorKind::Parse,
                    format!("could not serialize assistant output history: {error}"),
                )
            })?;
    }
    Ok(value)
}

fn build_request(
    messages: &[ChatMessage],
    tools: &[ToolDefinition],
) -> Result<CliRequest, LlmError> {
    for message in messages {
        if !message.images.is_empty() {
            return Err(LlmError::new_with_kind(
                LlmErrorKind::Config,
                "Claude Code CLI provider cannot send image history; choose a vision-capable provider or remove image messages",
            ));
        }
        if message.reasoning.is_some()
            || !message.reasoning_items.is_empty()
            || message
                .output_sequence
                .iter()
                .any(|item| matches!(item, OutputItem::Reasoning(_)))
        {
            return Err(LlmError::new_with_kind(
                LlmErrorKind::Config,
                "Claude Code CLI provider cannot replay provider-specific reasoning history; continue with the original provider or start a new conversation",
            ));
        }
    }

    let system_context = messages
        .iter()
        .filter(|message| message.role == ChatRole::System)
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    // System messages travel as system context, which can change without
    // invalidating the session's conversation history.
    let conversation = messages
        .iter()
        .filter(|message| message.role != ChatRole::System);
    let mut history = Vec::new();
    let mut normalized = Vec::new();
    for message in conversation {
        if let Some(stripped) = strip_reminders(message) {
            normalized.push((history.len(), message_value(&stripped)?));
        }
        history.push(message_value(message)?);
    }
    let tool_data = serde_json::to_value(tools).map_err(|error| {
        LlmError::new_with_kind(
            LlmErrorKind::Parse,
            format!("could not serialize Bone tool definitions: {error}"),
        )
    })?;

    let schema = json!({
        "type": "object",
        "properties": {
            "response": { "type": "string" },
            "tool_calls": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "arguments": { "type": "object", "additionalProperties": true }
                    },
                    "required": ["name", "arguments"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["response", "tool_calls"],
        "additionalProperties": false
    });

    Ok(CliRequest {
        system_context,
        history,
        normalized,
        tool_data,
        schema,
    })
}

struct ParsedResponse {
    text: String,
    tool_calls: Vec<(String, Value)>,
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
    cache_read_input_tokens: Option<u32>,
    cache_creation_input_tokens: Option<u32>,
    usage_present: bool,
    usage_cumulative: bool,
    /// Prompt size of the last API call when the CLI made several for one
    /// request. The top-level usage sums every call, which overstates context.
    final_call: Option<CallUsage>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct CallUsage {
    prompt_tokens: u32,
    cache_read_input_tokens: u32,
}

fn final_call_usage(usage: Option<&Value>) -> Option<CallUsage> {
    let last = usage?.get("iterations")?.as_array()?.last()?;
    let count = |name: &str| {
        last.get(name)
            .and_then(Value::as_u64)
            .map_or(0, |value| value.min(u32::MAX as u64) as u32)
    };
    let cache_read_input_tokens = count("cache_read_input_tokens");
    Some(CallUsage {
        prompt_tokens: count("input_tokens")
            .saturating_add(cache_read_input_tokens)
            .saturating_add(count("cache_creation_input_tokens")),
        cache_read_input_tokens,
    })
}

fn parse_cli_response(output: &[u8], tools: &[ToolDefinition]) -> Result<ParsedResponse, LlmError> {
    let envelope: Value = serde_json::from_slice(output).map_err(|error| {
        LlmError::new_with_kind(
            LlmErrorKind::Parse,
            format!("Claude Code CLI returned invalid JSON: {error}"),
        )
    })?;
    if envelope.get("is_error").and_then(Value::as_bool) == Some(true) {
        let detail = envelope
            .get("result")
            .and_then(Value::as_str)
            .or_else(|| envelope.get("error").and_then(Value::as_str))
            .unwrap_or_default();
        return Err(cli_reported_error(detail));
    }

    let payload = if let Some(structured) = envelope.get("structured_output") {
        structured.clone()
    } else if envelope.get("response").is_some() {
        envelope.clone()
    } else if let Some(result) = envelope.get("result") {
        if result.is_object() {
            result.clone()
        } else if let Some(result) = result.as_str() {
            serde_json::from_str(result).map_err(|error| {
                LlmError::new_with_kind(
                    LlmErrorKind::Parse,
                    format!("Claude Code CLI response did not contain structured output: {error}"),
                )
            })?
        } else {
            return Err(LlmError::new_with_kind(
                LlmErrorKind::Parse,
                "Claude Code JSON result did not contain structured output",
            ));
        }
    } else {
        return Err(LlmError::new_with_kind(
            LlmErrorKind::Parse,
            "Claude Code CLI JSON output did not include structured_output",
        ));
    };

    let usage = envelope
        .get("usage")
        .filter(|value| value.is_object())
        .or_else(|| payload.get("usage").filter(|value| value.is_object()))
        .or_else(|| {
            envelope
                .get("result")
                .and_then(|result| result.get("usage"))
                .filter(|value| value.is_object())
        });
    let (
        input_tokens,
        output_tokens,
        cache_read_input_tokens,
        cache_creation_input_tokens,
        usage_cumulative,
    ) = parse_usage(usage);

    let text = payload
        .get("response")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            LlmError::new_with_kind(
                LlmErrorKind::Parse,
                "Claude Code structured output is missing string field `response`",
            )
        })?
        .to_string();
    let returned_calls = payload
        .get("tool_calls")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            LlmError::new_with_kind(
                LlmErrorKind::Parse,
                "Claude Code structured output is missing array field `tool_calls`",
            )
        })?;
    let allowed: HashSet<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
    let mut tool_calls = Vec::with_capacity(returned_calls.len());
    for call in returned_calls {
        let name = call.get("name").and_then(Value::as_str).ok_or_else(|| {
            LlmError::new_with_kind(
                LlmErrorKind::Parse,
                "Claude Code returned a tool request without a string name",
            )
        })?;
        if !allowed.contains(name) {
            return Err(LlmError::new_with_kind(
                LlmErrorKind::Config,
                format!("Claude Code returned unknown Bone tool `{name}`; request rejected"),
            ));
        }
        let arguments = call
            .get("arguments")
            .filter(|value| value.is_object())
            .ok_or_else(|| {
                LlmError::new_with_kind(
                    LlmErrorKind::Parse,
                    format!("Claude Code returned non-object arguments for Bone tool `{name}`"),
                )
            })?;
        tool_calls.push((name.to_string(), arguments.clone()));
    }

    Ok(ParsedResponse {
        text,
        tool_calls,
        input_tokens,
        output_tokens,
        cache_read_input_tokens,
        cache_creation_input_tokens,
        usage_present: usage.is_some(),
        usage_cumulative,
        final_call: (!usage_cumulative)
            .then(|| final_call_usage(usage))
            .flatten(),
    })
}

fn parse_usage(
    usage: Option<&Value>,
) -> (Option<u32>, Option<u32>, Option<u32>, Option<u32>, bool) {
    let Some(usage) = usage else {
        return (None, None, None, None, false);
    };
    let token_count = |name: &str| {
        usage
            .get(name)
            .and_then(Value::as_u64)
            .map(|value| value.min(u32::MAX as u64) as u32)
    };
    let usage_cumulative = usage
        .get("cumulative")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || [
            "total_input_tokens",
            "total_output_tokens",
            "total_cache_read_input_tokens",
            "total_cache_creation_input_tokens",
        ]
        .iter()
        .any(|name| usage.get(*name).is_some());
    let choose_count = |per_request: &str, cumulative: &str| {
        if usage_cumulative {
            token_count(cumulative).or_else(|| token_count(per_request))
        } else {
            token_count(per_request).or_else(|| token_count(cumulative))
        }
    };
    let cache_creation_input_tokens = if usage_cumulative {
        token_count("total_cache_creation_input_tokens")
            .or_else(|| token_count("cache_creation_input_tokens"))
    } else {
        token_count("cache_creation_input_tokens").or_else(|| {
            usage
                .get("cache_creation")
                .and_then(Value::as_object)
                .map(|cache_creation| {
                    ["ephemeral_1h_input_tokens", "ephemeral_5m_input_tokens"]
                        .into_iter()
                        .filter_map(|name| cache_creation.get(name).and_then(Value::as_u64))
                        .fold(0u64, u64::saturating_add)
                        .min(u32::MAX as u64) as u32
                })
        })
    };
    (
        choose_count("input_tokens", "total_input_tokens"),
        choose_count("output_tokens", "total_output_tokens"),
        choose_count("cache_read_input_tokens", "total_cache_read_input_tokens"),
        cache_creation_input_tokens,
        usage_cumulative,
    )
}

fn cli_reported_error(detail: &str) -> LlmError {
    let lower = detail.to_ascii_lowercase();
    if ["auth", "login", "not logged", "unauthorized", "api key"]
        .iter()
        .any(|needle| lower.contains(needle))
    {
        LlmError::new_with_kind(
            LlmErrorKind::Auth,
            "Claude Code CLI authentication failed; sign in with the installed `claude` CLI",
        )
    } else if lower.contains("rate limit") || lower.contains("429") {
        LlmError::new_with_kind(
            LlmErrorKind::RateLimit,
            "Claude Code CLI reported a rate limit; retry after the limit resets",
        )
    } else {
        LlmError::new_with_kind(
            LlmErrorKind::Server(500),
            "Claude Code CLI reported an error; check its local configuration and logs",
        )
    }
}

#[derive(Clone, Default)]
struct UsageSnapshot {
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
    cache_read_input_tokens: Option<u32>,
    cache_creation_input_tokens: Option<u32>,
}

struct ClaudeCodeSession {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    stderr_task: Option<JoinHandle<String>>,
    executable: PathBuf,
    model: String,
    conversation_id: Option<i64>,
    cache_scope: Option<String>,
    /// State the CLI conversation already holds. `None` until the first
    /// successful exchange.
    synchronized: Option<SynchronizedState>,
    in_flight: bool,
    cumulative_usage: Option<UsageSnapshot>,
    // Keep the working directory alive for the whole CLI session. It is
    // declared after the child so the process is dropped before the directory.
    _isolated_dir: TempDir,
}

impl ClaudeCodeSession {
    fn matches(&self, executable: &PathBuf, model: &str, context: &ProviderRequestContext) -> bool {
        self.executable == *executable
            && self.model == model
            && self.conversation_id == context.conversation_id
            && self.cache_scope == context.cache_scope
    }

    /// Plan how to bring this session up to `request`, or `None` when the
    /// history diverged too far and the session must be discarded.
    fn delta_for(&self, request: &CliRequest) -> Option<SessionDelta> {
        let Some(synced) = &self.synchronized else {
            // Fresh session: the system prompt already carries the context.
            return Some(SessionDelta {
                fresh: true,
                history_start: 0,
                retracted: 0,
                system_context_changed: false,
                tools_changed: true,
            });
        };
        let (history_start, retracted) = synced.history_position(request)?;
        Some(SessionDelta {
            fresh: false,
            history_start,
            retracted,
            system_context_changed: synced.system_context != request.system_context,
            tools_changed: synced.tool_data != request.tool_data,
        })
    }

    fn normalize_usage(&mut self, response: &mut ParsedResponse) {
        if !response.usage_cumulative {
            return;
        }
        let current = UsageSnapshot {
            input_tokens: response.input_tokens,
            output_tokens: response.output_tokens,
            cache_read_input_tokens: response.cache_read_input_tokens,
            cache_creation_input_tokens: response.cache_creation_input_tokens,
        };
        let previous = self.cumulative_usage.as_ref();
        response.input_tokens = delta_count(
            current.input_tokens,
            previous.and_then(|usage| usage.input_tokens),
        );
        response.output_tokens = delta_count(
            current.output_tokens,
            previous.and_then(|usage| usage.output_tokens),
        );
        response.cache_read_input_tokens = delta_count(
            current.cache_read_input_tokens,
            previous.and_then(|usage| usage.cache_read_input_tokens),
        );
        response.cache_creation_input_tokens = delta_count(
            current.cache_creation_input_tokens,
            previous.and_then(|usage| usage.cache_creation_input_tokens),
        );
        self.cumulative_usage = Some(current);
    }

    async fn exchange(
        &mut self,
        prompt: &str,
        tools: &[ToolDefinition],
    ) -> Result<ParsedResponse, LlmError> {
        let input = json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": prompt,
            },
        });
        let mut encoded = serde_json::to_vec(&input).map_err(|error| {
            LlmError::new_with_kind(
                LlmErrorKind::Parse,
                format!("could not encode Claude Code stream input: {error}"),
            )
        })?;
        encoded.push(b'\n');
        self.stdin.write_all(&encoded).await.map_err(|error| {
            LlmError::new_with_kind(
                LlmErrorKind::Connection,
                format!("could not send the prompt to Claude Code CLI stdin: {error}"),
            )
        })?;
        self.stdin.flush().await.map_err(|error| {
            LlmError::new_with_kind(
                LlmErrorKind::Connection,
                format!("could not flush Claude Code CLI stdin: {error}"),
            )
        })?;

        loop {
            let mut line = String::new();
            let bytes_read = self.stdout.read_line(&mut line).await.map_err(|error| {
                LlmError::new_with_kind(
                    LlmErrorKind::Connection,
                    format!("could not read Claude Code CLI output: {error}"),
                )
            })?;
            if bytes_read == 0 {
                return Err(self.eof_error().await);
            }
            if line.trim().is_empty() {
                continue;
            }
            let event: Value = serde_json::from_str(&line).map_err(|error| {
                LlmError::new_with_kind(
                    LlmErrorKind::Parse,
                    format!("Claude Code CLI returned invalid stream JSON: {error}"),
                )
            })?;
            match event.get("type").and_then(Value::as_str) {
                Some("result") => return parse_cli_response(line.as_bytes(), tools),
                Some("error") => {
                    let detail = event
                        .get("error")
                        .and_then(Value::as_str)
                        .or_else(|| event.get("message").and_then(Value::as_str))
                        .or_else(|| event.get("result").and_then(Value::as_str))
                        .unwrap_or_default();
                    return Err(cli_reported_error(detail));
                }
                Some(_) => {}
                None => {
                    return Err(LlmError::new_with_kind(
                        LlmErrorKind::Parse,
                        "Claude Code stream event did not include a string `type`",
                    ));
                }
            }
        }
    }

    async fn eof_error(&mut self) -> LlmError {
        let status = match self.child.try_wait() {
            Ok(Some(status)) => Some(status),
            Ok(None) => {
                let _ = self.child.kill().await;
                self.child.wait().await.ok()
            }
            Err(_) => None,
        };
        let detail = match self.stderr_task.take() {
            Some(task) => task.await.unwrap_or_default(),
            None => String::new(),
        };
        if let Some(status) = status.filter(|status| !status.success()) {
            let reported = cli_reported_error(&detail);
            if matches!(reported.kind, LlmErrorKind::Auth | LlmErrorKind::RateLimit) {
                return reported;
            }
            let detail = detail.trim();
            let message = if detail.is_empty() {
                format!(
                    "Claude Code CLI exited with status {}; check its local configuration and logs",
                    status
                )
            } else {
                format!(
                    "Claude Code CLI exited with status {status}: {detail}; check its local configuration and logs"
                )
            };
            return LlmError::new_with_kind(LlmErrorKind::Server(500), message);
        }
        LlmError::new_with_kind(
            LlmErrorKind::Connection,
            "Claude Code CLI closed its stream before returning a result",
        )
    }
}

struct SynchronizedState {
    system_context: String,
    tool_data: Value,
    history: Vec<Value>,
    normalized: Vec<(usize, Value)>,
}

impl SynchronizedState {
    fn from_request(request: CliRequest) -> Self {
        Self {
            system_context: request.system_context,
            tool_data: request.tool_data,
            history: request.history,
            normalized: request.normalized,
        }
    }

    /// Returns where `request.history` continues past what the session holds,
    /// plus how many previously sent messages the model must disregard.
    fn history_position(&self, request: &CliRequest) -> Option<(usize, usize)> {
        let synced_len = self.history.len();
        if request.history.len() >= synced_len && request.history[..synced_len] == self.history[..]
        {
            return Some((synced_len, 0));
        }
        // Compare without request-only reminders, which later requests rebuild
        // the same messages without.
        let common = self
            .normalized
            .iter()
            .zip(&request.normalized)
            .take_while(|((_, synced), (_, requested))| synced == requested)
            .count();
        let retracted = self.normalized.len() - common;
        if retracted > 0 && (common == 0 || retracted > MAX_RETRACTED_MESSAGES) {
            return None;
        }
        let start = match common {
            0 => 0,
            _ => request.normalized[common - 1].0 + 1,
        };
        Some((start, retracted))
    }
}

fn delta_count(current: Option<u32>, previous: Option<u32>) -> Option<u32> {
    current.map(|current| current.saturating_sub(previous.unwrap_or_default()))
}

impl ClaudeCodeProvider {
    async fn chat_stream_impl(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
        context: ProviderRequestContext,
    ) -> Result<ResponseStream, LlmError> {
        let request = build_request(&messages, &tools)?;
        let mut session_guard = self.session.lock().await;
        let delta = session_guard.as_ref().and_then(|session| {
            (!session.in_flight && session.matches(&self.executable, &self.model, &context))
                .then(|| session.delta_for(&request))
                .flatten()
        });
        let delta = match delta {
            Some(delta) => delta,
            None => {
                *session_guard = Some(self.start_session(&request, &context)?);
                session_guard
                    .as_ref()
                    .and_then(|session| session.delta_for(&request))
                    .expect("a fresh Claude session accepts any history")
            }
        };
        let prompt = request.prompt_for_delta(&delta);
        let result = {
            let session = session_guard.as_mut().expect("Claude session disappeared");
            session.in_flight = true;
            tokio::time::timeout(
                self.request_timeout_value(),
                session.exchange(&prompt, &tools),
            )
            .await
        };
        let mut response = match result {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                *session_guard = None;
                return Err(error);
            }
            Err(_) => {
                *session_guard = None;
                return Err(LlmError::new_with_kind(
                    LlmErrorKind::Timeout,
                    "Claude Code CLI request timed out; the session was discarded before retry",
                ));
            }
        };

        let session = session_guard
            .as_mut()
            .expect("Claude session disappeared after a successful response");
        session.in_flight = false;
        session.normalize_usage(&mut response);
        session.synchronized = Some(SynchronizedState::from_request(request));

        let mut events = Vec::new();
        if !response.text.is_empty() {
            events.push(ChatEvent::TextDelta(response.text));
        }
        for (index, (name, arguments)) in response.tool_calls.into_iter().enumerate() {
            let sequence = NEXT_CALL_ID.fetch_add(1, Ordering::Relaxed);
            events.push(ChatEvent::ToolCall(ToolCall {
                id: format!("claude-code-{}-{sequence}-{index}", std::process::id()),
                name,
                arguments,
            }));
        }
        if response.usage_present {
            let prompt_tokens = response
                .input_tokens
                .unwrap_or_default()
                .saturating_add(response.cache_read_input_tokens.unwrap_or_default())
                .saturating_add(response.cache_creation_input_tokens.unwrap_or_default());
            let cached_tokens = response.cache_read_input_tokens;
            // Bone reads context size from the last usage event. When the CLI
            // made several API calls, report the earlier calls first so totals
            // stay exact, then the final call, whose prompt is the context.
            let final_call = response
                .final_call
                .filter(|call| call.prompt_tokens > 0 && call.prompt_tokens < prompt_tokens);
            match final_call {
                Some(call) => {
                    events.push(ChatEvent::TokenUsage {
                        prompt_tokens: prompt_tokens - call.prompt_tokens,
                        completion_tokens: 0,
                        cached_tokens: cached_tokens
                            .map(|read| read.saturating_sub(call.cache_read_input_tokens)),
                        cost: None,
                    });
                    events.push(ChatEvent::TokenUsage {
                        prompt_tokens: call.prompt_tokens,
                        completion_tokens: response.output_tokens.unwrap_or_default(),
                        cached_tokens: cached_tokens.map(|_| call.cache_read_input_tokens),
                        cost: None,
                    });
                }
                None => events.push(ChatEvent::TokenUsage {
                    prompt_tokens,
                    completion_tokens: response.output_tokens.unwrap_or_default(),
                    cached_tokens,
                    cost: None,
                }),
            }
        }
        Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
    }

    fn start_session(
        &self,
        request: &CliRequest,
        context: &ProviderRequestContext,
    ) -> Result<ClaudeCodeSession, LlmError> {
        let isolated_dir = tempfile::Builder::new()
            .prefix("bone-claude-code-")
            .tempdir()
            .map_err(|error| {
                LlmError::new_with_kind(
                    LlmErrorKind::Connection,
                    format!("could not create an isolated Claude CLI working directory: {error}"),
                )
            })?;
        let mut command = tokio::process::Command::new(&self.executable);
        command
            .arg("-p")
            .arg("--output-format")
            .arg("stream-json")
            .arg("--verbose")
            .arg("--input-format")
            .arg("stream-json")
            .arg("--json-schema")
            .arg(request.schema.to_string())
            .arg("--tools")
            .arg("")
            .arg("--system-prompt")
            .arg(request.system_prompt())
            .arg("--model")
            .arg(&self.model)
            .arg("--no-session-persistence")
            .arg("--system-prompt-snapshot")
            .arg("on")
            .arg("--strict-mcp-config")
            .arg("--mcp-config")
            .arg(EMPTY_MCP_CONFIG)
            .arg("--setting-sources")
            .arg("")
            .arg("--disable-slash-commands")
            .arg("--no-chrome")
            .current_dir(isolated_dir.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                LlmError::new_with_kind(
                    LlmErrorKind::Config,
                    "Claude Code CLI `claude` was not found on PATH; install Claude Code and sign in",
                )
            } else {
                LlmError::new_with_kind(
                    LlmErrorKind::Connection,
                    format!("could not start Claude Code CLI: {error}"),
                )
            }
        })?;
        let stdin = child.stdin.take().ok_or_else(|| {
            LlmError::new_with_kind(
                LlmErrorKind::Connection,
                "could not open Claude Code CLI stdin",
            )
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            LlmError::new_with_kind(
                LlmErrorKind::Connection,
                "could not open Claude Code CLI stdout",
            )
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            LlmError::new_with_kind(
                LlmErrorKind::Connection,
                "could not open Claude Code CLI stderr",
            )
        })?;
        let stderr_task = tokio::spawn(async move {
            let mut stderr = stderr.take(8192);
            let mut bytes = Vec::new();
            let _ = stderr.read_to_end(&mut bytes).await;
            String::from_utf8_lossy(&bytes).into_owned()
        });
        Ok(ClaudeCodeSession {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            stderr_task: Some(stderr_task),
            executable: self.executable.clone(),
            model: self.model.clone(),
            conversation_id: context.conversation_id,
            cache_scope: context.cache_scope.clone(),
            synchronized: None,
            in_flight: false,
            cumulative_usage: None,
            _isolated_dir: isolated_dir,
        })
    }
}

#[cfg(all(test, unix))]
#[path = "claude_code_tests.rs"]
mod tests;

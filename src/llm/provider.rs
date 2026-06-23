use async_trait::async_trait;
use futures_util::Stream;
use serde::{Deserialize, Serialize};
use std::{error::Error, fmt, pin::Pin};

use crate::tools::{ToolCall, ToolDefinition, ToolResult};

/// A boxed async stream of provider events from an LLM provider.
pub type ResponseStream = Pin<Box<dyn Stream<Item = Result<ChatEvent, LlmError>> + Send>>;

/// Provider-neutral chat roles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    System,
    User,
    Assistant,
    Tool,
}

impl ChatRole {
    /// Return the role as a lowercase string ("system", "user", "assistant", "tool").
    pub fn as_str(self) -> &'static str {
        match self {
            ChatRole::System => "system",
            ChatRole::User => "user",
            ChatRole::Assistant => "assistant",
            ChatRole::Tool => "tool",
        }
    }
}

/// Provider-neutral chat message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Reasoning/thinking content produced during the turn. Some providers
    /// require it be echoed back when the turn involved tool calls; the wire
    /// field to round-trip it under is carried opaquely in [`Reasoning`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Reasoning>,
}

/// Provider-neutral reasoning/thinking content captured during a turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reasoning {
    pub text: String,
    /// Wire field this must be echoed back under on the next request, if the
    /// provider requires round-tripping it (else `None`). Opaque to the core.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub echo_field: Option<String>,
}

impl ChatMessage {
    pub fn new(role: ChatRole, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
            reasoning: None,
        }
    }

    pub fn assistant_with_tools(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            tool_calls,
            ..Self::new(ChatRole::Assistant, content)
        }
    }

    pub fn tool(result: ToolResult) -> Self {
        Self {
            role: ChatRole::Tool,
            content: result.content,
            tool_calls: Vec::new(),
            tool_call_id: Some(result.call_id),
            name: Some(result.name),
            reasoning: None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum ChatEvent {
    TextDelta(String),
    /// Reasoning/thinking token. `echo_field` names the wire field it must be
    /// round-tripped under on the next request, if the provider requires it.
    ReasoningDelta {
        text: String,
        echo_field: Option<String>,
    },
    ToolCall(ToolCall),
    /// Token usage from the provider's response (real counts, not estimates).
    TokenUsage {
        prompt_tokens: u32,
        completion_tokens: u32,
        cached_tokens: Option<u32>,
        cost: Option<f64>,
    },
}

/// Classification of an LLM error.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum LlmErrorKind {
    /// Server unreachable (DNS, connect timeout, etc.).
    Connection,
    /// Provider request or stream timed out.
    Timeout,
    /// Authentication/authorization error (401/403).
    Auth,
    /// Rate-limited (429).
    RateLimit,
    /// Server-side error with the HTTP status code. Reserved for retry logic.
    Server(u16),
    /// Malformed response from the server.
    Parse,
    /// Bad provider configuration.
    Config,
}

/// Small, owned error type so provider streams can be boxed cleanly.
///
/// Fields are public but primarily accessed via [`Display`]/[`Error`];
/// [`kind`](Self::kind) and [`message`](Self::message) are available for
/// retry logic and error classification.
#[derive(Debug, Clone)]
pub struct LlmError {
    pub kind: LlmErrorKind,
    /// Human-readable error message.
    pub message: String,
}

impl LlmError {
    pub fn new_with_kind(kind: LlmErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for LlmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl Error for LlmError {}

impl From<reqwest::Error> for LlmError {
    fn from(err: reqwest::Error) -> Self {
        let kind = match err.status() {
            Some(code) if code.as_u16() == 401 || code.as_u16() == 403 => LlmErrorKind::Auth,
            Some(code) if code.as_u16() == 429 => LlmErrorKind::RateLimit,
            Some(code) if code.is_server_error() => LlmErrorKind::Server(code.as_u16()),
            _ => {
                if err.is_timeout() {
                    LlmErrorKind::Timeout
                } else if err.is_connect() || err.is_request() {
                    LlmErrorKind::Connection
                } else {
                    LlmErrorKind::Config
                }
            }
        };
        Self {
            kind,
            message: err.to_string(),
        }
    }
}

impl From<serde_json::Error> for LlmError {
    fn from(err: serde_json::Error) -> Self {
        Self {
            kind: LlmErrorKind::Parse,
            message: err.to_string(),
        }
    }
}
/// Map an HTTP status code to an [`LlmErrorKind`].
///
/// Shared across all HTTP-based providers.
pub fn http_status_to_error_kind(status: reqwest::StatusCode) -> LlmErrorKind {
    match status.as_u16() {
        401 | 403 => LlmErrorKind::Auth,
        429 => LlmErrorKind::RateLimit,
        code if code >= 500 => LlmErrorKind::Server(code),
        _ => LlmErrorKind::Config,
    }
}

/// The only interface providers need to implement.
///
/// To add a provider:
/// 1. Create `src/llm/providers/your_provider.rs`.
/// 2. Implement this trait.
/// 3. Register it in `src/llm/providers/mod.rs`.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Stable provider key used in config/CLI, e.g. `local`, `glm`, `openai`.
    fn id(&self) -> &str;

    /// Human-readable provider name.
    fn name(&self) -> &str;

    /// Currently selected model.
    fn model(&self) -> &str;

    /// Change the model to use for subsequent requests.
    fn set_model(&mut self, model: String);

    /// Cap the number of output tokens for subsequent requests. `None` clears
    /// any cap. Default is a no-op for providers that don't support it; used by
    /// sub-agent runs (e.g. context compaction) to bound a model's output.
    fn set_max_tokens(&mut self, _max_tokens: Option<u32>) {}

    /// Send messages and stream provider-neutral response events.
    async fn chat_stream(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<ResponseStream, LlmError>;

    /// Validate provider setup before first use.
    async fn validate(&self) -> Result<(), LlmError> {
        Ok(())
    }
}

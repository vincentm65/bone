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
    /// Reasoning/thinking content from DeepSeek V4 thinking mode.
    /// Must be passed back when the assistant turn involved tool calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

impl ChatMessage {
    pub fn new(role: ChatRole, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
            reasoning_content: None,
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
            reasoning_content: None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum ChatEvent {
    TextDelta(String),
    /// Reasoning/thinking token from DeepSeek V4 thinking mode.
    ReasoningDelta(String),
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
    /// Create an error with [`LlmErrorKind::Config`] kind.
    ///
    /// Use [`new_with_kind`](Self::new_with_kind) for specific error
    /// classification.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            kind: LlmErrorKind::Config,
            message: message.into(),
        }
    }

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
                } else if err.is_connect() {
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

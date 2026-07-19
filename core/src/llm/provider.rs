//! The `LlmProvider` trait, chat roles, streaming events, and error types.
//!
//! Wire-format types (`ChatRole`, `ChatMessage`, etc.) are re-exported from
//! `bone-protocol`; only the `LlmProvider` trait, `ChatEvent`, `LlmError`,
//! and related core-local types stay here.

use async_trait::async_trait;
use futures_util::Stream;
use std::{
    error::Error,
    fmt,
    pin::Pin,
    sync::{Arc, OnceLock},
};

use crate::tools::{TRUNCATED_ARGS_KEY, ToolDefinition};

/// Parse streamed tool arguments without discarding malformed/truncated JSON.
/// Empty input is the canonical no-argument object; invalid input is retained
/// under the marker consumed by tool validation.
pub(crate) fn parse_tool_arguments(raw: &str) -> serde_json::Value {
    if raw.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(raw).unwrap_or_else(|_| serde_json::json!({ TRUNCATED_ARGS_KEY: raw }))
    }
}

// Re-export wire-format types from protocol.
pub use bone_protocol::{
    ChatMessage, ChatRole, ImageData, OutputItem, Reasoning, ReasoningItem, ToolCall, ToolResult,
};

/// A boxed async stream of provider events from an LLM provider.
pub type ResponseStream = Pin<Box<dyn Stream<Item = Result<ChatEvent, LlmError>> + Send>>;

#[derive(Debug, Clone)]
pub enum ChatEvent {
    TextDelta(String),
    ReasoningDelta {
        text: String,
        echo_field: Option<String>,
    },
    EncryptedReasoning {
        id: String,
        encrypted_content: String,
    },
    ToolCall(ToolCall),
    TokenUsage {
        prompt_tokens: u32,
        completion_tokens: u32,
        cached_tokens: Option<u32>,
        cost: Option<f64>,
    },
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum LlmErrorKind {
    Connection,
    Timeout,
    Auth,
    RateLimit,
    Server(u16),
    Parse,
    Config,
}

#[derive(Debug, Clone)]
pub struct LlmError {
    pub kind: LlmErrorKind,
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

pub fn http_status_to_error_kind(status: reqwest::StatusCode) -> LlmErrorKind {
    match status.as_u16() {
        401 | 403 => LlmErrorKind::Auth,
        429 => LlmErrorKind::RateLimit,
        code if code >= 500 => LlmErrorKind::Server(code),
        _ => LlmErrorKind::Config,
    }
}

/// Build a `reqwest::Client` tuned for SSE streaming: a 10s connect timeout
/// and a 120s idle/read timeout (NOT a total-request timeout, so a long
/// reasoning stream is never killed mid-turn).
pub fn streaming_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .read_timeout(std::time::Duration::from_secs(120))
        .build()
        .unwrap_or_default()
}

/// Build a user-facing error message from a failed HTTP response, surfacing
/// the backend's body (capped at 2000 chars) so the *why* isn't hidden behind
/// a bare status code.
pub fn http_error(status: reqwest::StatusCode, url: &str, body: &str) -> LlmError {
    let detail = body.trim();
    let msg = if detail.is_empty() {
        format!("HTTP {status} from {url}")
    } else {
        let capped: String = detail.chars().take(2000).collect();
        format!("HTTP {status} from {url}: {capped}")
    };
    LlmError::new_with_kind(http_status_to_error_kind(status), msg)
}

#[derive(Debug, Clone, Default)]
pub struct ProviderRequestContext {
    pub conversation_id: Option<i64>,
    /// Backend routing state scoped to one submitted user turn. Providers may
    /// capture it from the first response and replay it for retries and tool
    /// rounds, but it must not be reused by a later user turn.
    pub turn_state: Option<Arc<OnceLock<String>>>,
}

#[async_trait]
pub trait LlmProvider: Send + Sync {
    fn id(&self) -> &str;
    fn name(&self) -> &str;
    fn model(&self) -> &str;
    fn set_model(&mut self, model: String);
    fn set_max_tokens(&mut self, _max_tokens: Option<u32>) {}
    /// Maximum model context in tokens, or `None` when unknown.
    fn context_window_tokens(&self) -> Option<u64> {
        None
    }

    async fn chat_stream(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<ResponseStream, LlmError>;

    async fn chat_stream_with_context(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
        _context: ProviderRequestContext,
    ) -> Result<ResponseStream, LlmError> {
        self.chat_stream(messages, tools).await
    }

    async fn validate(&self) -> Result<(), LlmError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::parse_tool_arguments;
    use crate::tools::TRUNCATED_ARGS_KEY;
    use serde_json::json;

    #[test]
    fn tool_argument_contract() {
        assert_eq!(parse_tool_arguments(""), json!({}));
        assert_eq!(parse_tool_arguments(" \n\t"), json!({}));
        assert_eq!(
            parse_tool_arguments(r#"{"path":"x"}"#),
            json!({"path": "x"})
        );
        assert_eq!(
            parse_tool_arguments(r#"{"path":"x"#),
            json!({TRUNCATED_ARGS_KEY: r#"{"path":"x"#})
        );
    }
}

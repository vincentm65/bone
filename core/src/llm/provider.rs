//! The `LlmProvider` trait, chat roles, streaming events, and error types.
//!
//! Wire-format types (`ChatRole`, `ChatMessage`, etc.) are re-exported from
//! `bone-protocol`; only the `LlmProvider` trait, `ChatEvent`, `LlmError`,
//! and related core-local types stay here.

use async_trait::async_trait;
use futures_util::Stream;
use std::{error::Error, fmt, pin::Pin};

use crate::tools::ToolDefinition;

// Re-export wire-format types from protocol.
pub use bone_protocol::{ChatMessage, ChatRole, ImageData, OutputItem, Reasoning, ReasoningItem, ToolCall, ToolResult};

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

#[derive(Debug, Clone, Default)]
pub struct ProviderRequestContext {
    pub conversation_id: Option<i64>,
}

#[async_trait]
pub trait LlmProvider: Send + Sync {
    fn id(&self) -> &str;
    fn name(&self) -> &str;
    fn model(&self) -> &str;
    fn set_model(&mut self, model: String);
    fn set_max_tokens(&mut self, _max_tokens: Option<u32>) {}

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

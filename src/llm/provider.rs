use async_trait::async_trait;
use futures_util::Stream;
use serde::{Deserialize, Serialize};
use std::{error::Error, fmt, pin::Pin};

/// A boxed async stream of text chunks from an LLM provider.
pub type ResponseStream = Pin<Box<dyn Stream<Item = Result<String, LlmError>> + Send>>;

/// Provider-neutral chat roles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    System,
    User,
    Assistant,
}

/// Provider-neutral chat message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
}

/// Classification of an LLM error.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum LlmErrorKind {
    /// Server unreachable (DNS, connect timeout, etc.).
    Connection,
    /// Authentication/authorization error (401/403).
    Auth,
    /// Rate-limited (429).
    RateLimit,
    /// Server-side error with the HTTP status code.
    #[allow(dead_code)] // Status code used for construction; will be read in retry logic
    Server(u16),
    /// Malformed response from the server.
    Parse,
    /// Bad provider configuration.
    Config,
}

/// Small, owned error type so provider streams can be boxed cleanly.
#[derive(Debug, Clone)]
#[allow(dead_code)] // Fields used for construction; will be read in retry/error-display logic
pub struct LlmError {
    pub kind: LlmErrorKind,
    message: String,
}

impl LlmError {
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
                if err.is_connect() || err.is_timeout() {
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

    /// Send messages and stream response text chunks.
    async fn chat_stream(&self, messages: Vec<ChatMessage>) -> Result<ResponseStream, LlmError>;

    /// Validate provider setup before first use.
    async fn validate(&self) -> Result<(), LlmError> {
        Ok(())
    }
}

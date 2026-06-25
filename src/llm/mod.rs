//! Provider-neutral LLM layer: chat types, streaming events, and token tracking.

pub mod prompts;
pub mod provider;
pub mod providers;
pub mod token_tracker;

pub use provider::{
    ChatEvent, ChatMessage, ChatRole, ImageData, LlmError, LlmErrorKind, LlmProvider, Reasoning,
    ResponseStream,
};
pub use token_tracker::{TokenStats, format_tokens};

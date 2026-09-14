//! Provider-neutral LLM layer: chat types, streaming events, and token tracking.

pub mod prompts;
pub mod provider;
pub mod providers;
pub mod token_tracker;

pub use provider::{
    ChatEvent, ChatMessage, ChatRole, IMAGE_RELAY_PREFIX, ImageData, LlmError, LlmErrorKind,
    LlmProvider, OutputItem, Reasoning, ReasoningItem, ResponseStream,
};
pub use token_tracker::{
    TokenStats, format_tokens, estimate_image_tokens, parse_image_dimensions, ImageTokenProfile,
};

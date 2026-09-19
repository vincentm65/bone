//! Provider-neutral LLM layer: chat types, streaming events, and token tracking.

pub mod image_normalize;
pub mod prompts;
pub mod provider;
pub mod providers;
pub mod token_tracker;

pub use image_normalize::normalize_images_in_messages;

pub use provider::{
    ChatEvent, ChatMessage, ChatRole, IMAGE_RELAY_PREFIX, ImageData, LlmError, LlmErrorKind,
    LlmProvider, OutputItem, Reasoning, ReasoningItem, ResponseStream,
};
pub use token_tracker::{
    ImageTokenProfile, TokenStats, estimate_image_tokens, format_tokens, parse_image_dimensions,
};

//! Model providers.

mod openai;
mod sse;

pub use openai::OpenAiProvider;

use bone_proto::types::{ChatMessage, ToolCall, Usage};
use futures_util::future::BoxFuture;

use crate::tools::ToolSpec;

pub struct CompletionRequest<'a> {
    pub messages: &'a [ChatMessage],
    pub tools: &'a [ToolSpec],
}

/// Incremental output while a completion streams.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delta {
    Text(String),
    Reasoning(String),
}

/// A finished model response.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Completion {
    pub content: String,
    pub reasoning: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Option<Usage>,
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ProviderError(pub String);

pub type DeltaSink<'a> = &'a mut (dyn FnMut(Delta) + Send);

pub trait Provider: Send + Sync {
    /// Stream one completion, reporting deltas as they arrive. Dropping the
    /// future cancels the request.
    fn complete<'a>(
        &'a self,
        req: CompletionRequest<'a>,
        on_delta: DeltaSink<'a>,
    ) -> BoxFuture<'a, Result<Completion, ProviderError>>;
}

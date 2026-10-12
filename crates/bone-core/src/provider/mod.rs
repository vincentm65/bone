//! Model providers.

mod anthropic;
mod openai;
mod openai_responses;
pub(crate) mod sse;

pub use anthropic::AnthropicProvider;
pub use openai::OpenAiProvider;
pub use openai_responses::OpenAiResponsesProvider;

use std::sync::Arc;

use bone_proto::types::{ChatMessage, ProviderApi, ReasoningState, ToolCall, Usage};
use futures_util::future::BoxFuture;

use crate::tools::ToolSpec;

pub struct CompletionRequest<'a> {
    pub session_id: &'a str,
    pub messages: &'a [ChatMessage],
    pub tools: &'a [ToolSpec],
    /// How many model calls this one is nested in (a Lua provider or tool
    /// calling `bone.model`); 0 for the agent's own calls.
    pub depth: u32,
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
    pub reasoning_state: Option<ReasoningState>,
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ProviderError(pub String);

pub type DeltaSink<'a> = &'a mut (dyn FnMut(Delta) + Send);

pub trait Provider: Send + Sync {
    /// Whether assistant reasoning is replayed as input on subsequent calls.
    fn replays_reasoning(&self) -> bool {
        false
    }

    /// Stream one completion, reporting deltas as they arrive. Dropping the
    /// future cancels the request.
    fn complete<'a>(
        &'a self,
        req: CompletionRequest<'a>,
        on_delta: DeltaSink<'a>,
    ) -> BoxFuture<'a, Result<Completion, ProviderError>>;
}

/// What turns use when no provider is configured (a first run, before
/// /setup): every call fails with how to set one up.
pub struct Unconfigured;

impl Provider for Unconfigured {
    fn complete<'a>(
        &'a self,
        _req: CompletionRequest<'a>,
        _on_delta: DeltaSink<'a>,
    ) -> BoxFuture<'a, Result<Completion, ProviderError>> {
        Box::pin(async {
            Err(ProviderError(
                "no model provider is configured: run /setup (or add one to core.lua)".into(),
            ))
        })
    }
}

/// The URL requests go to: the configured one, or the API's default.
pub fn base_url(p: &crate::config::ProviderConfig) -> &str {
    match p.base_url.trim_end_matches('/') {
        "" if p.api == ProviderApi::Anthropic => anthropic::DEFAULT_URL,
        url => url,
    }
}

/// No provider is set up: no Lua type, and nowhere to send requests.
pub fn unconfigured(p: &crate::config::ProviderConfig) -> bool {
    p.kind.is_none() && base_url(p).is_empty()
}

/// The built-in provider for a config, by its `api`.
pub fn built_in(p: crate::config::ProviderConfig) -> Arc<dyn Provider> {
    match p.api {
        ProviderApi::Completions => Arc::new(OpenAiProvider::new(p)),
        ProviderApi::Responses => Arc::new(OpenAiResponsesProvider::new(p)),
        ProviderApi::Anthropic => Arc::new(AnthropicProvider::new(p)),
    }
}

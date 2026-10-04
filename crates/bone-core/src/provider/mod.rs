//! Model providers.

mod openai;
pub(crate) mod sse;

pub use openai::OpenAiProvider;

use bone_proto::types::{ChatMessage, ToolCall, Usage};
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

/// The provider for a config: Lua (with a type), none (no URL), else
/// OpenAI-compatible.
pub fn unconfigured(p: &crate::config::ProviderConfig) -> bool {
    p.kind.is_none() && p.base_url.is_empty()
}

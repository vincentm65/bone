//! Model providers.

mod anthropic;
mod openai;
mod openai_responses;
pub(crate) mod sse;

pub(crate) use openai::http_client;

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

/// The wire format: the configured one, or Anthropic's and OpenAI's own
/// APIs by host (or a Claude model with no URL), else Chat Completions.
pub fn api(p: &crate::config::ProviderConfig) -> ProviderApi {
    if let Some(api) = p.api {
        return api;
    }
    let host = reqwest::Url::parse(&p.base_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned));
    match host.as_deref() {
        Some("api.anthropic.com") => ProviderApi::Anthropic,
        Some("api.openai.com") => ProviderApi::Responses,
        None if p.base_url.is_empty() && p.model.starts_with("claude-") => ProviderApi::Anthropic,
        _ => ProviderApi::Completions,
    }
}

/// The URL requests go to: the configured one, or the API's default.
pub fn base_url(p: &crate::config::ProviderConfig) -> &str {
    match p.base_url.trim_end_matches('/') {
        "" if api(p) == ProviderApi::Anthropic => anthropic::DEFAULT_URL,
        url => url,
    }
}

/// No provider is set up: no Lua type, and nowhere to send requests.
pub fn unconfigured(p: &crate::config::ProviderConfig) -> bool {
    p.kind.is_none() && base_url(p).is_empty()
}

/// The built-in provider for a config, by its `api`.
pub fn built_in(p: crate::config::ProviderConfig) -> Arc<dyn Provider> {
    Arc::new(BuiltIn {
        http: http_client(),
        config: p,
    })
}

/// A built-in provider: the wire format its `api` names, over an HTTP
/// client whose connections every request reuses.
pub(crate) struct BuiltIn {
    pub http: reqwest::Client,
    pub config: crate::config::ProviderConfig,
}

impl Provider for BuiltIn {
    fn replays_reasoning(&self) -> bool {
        match api(&self.config) {
            ProviderApi::Completions => self.config.replay_reasoning,
            ProviderApi::Responses => false,
            ProviderApi::Anthropic => true,
        }
    }

    fn complete<'a>(
        &'a self,
        req: CompletionRequest<'a>,
        on_delta: DeltaSink<'a>,
    ) -> BoxFuture<'a, Result<Completion, ProviderError>> {
        let (http, config) = (&self.http, &self.config);
        Box::pin(async move {
            openai::check_images(config, req.messages)?;
            match api(config) {
                ProviderApi::Completions => openai::stream(http, config, req, on_delta).await,
                ProviderApi::Responses => {
                    openai_responses::stream(http, config, req, on_delta).await
                }
                ProviderApi::Anthropic => anthropic::stream(http, config, req, on_delta).await,
            }
        })
    }
}

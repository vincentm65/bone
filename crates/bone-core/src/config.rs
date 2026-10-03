//! Core configuration. Plain data; `scripting::load` builds it from
//! `core.lua` and `BONE_*` overrides.

use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct CoreConfig {
    pub provider: ProviderConfig,
    /// Replaces the built-in system prompt. The working directory is always
    /// appended.
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// Where sessions are stored.
    pub data_dir: PathBuf,
}

/// An OpenAI-compatible `/chat/completions` endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConfig {
    /// e.g. `https://api.deepseek.com/v1` or `http://localhost:8081/v1`.
    pub base_url: String,
    pub model: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    /// Send `stream_options.include_usage`. Some servers reject it.
    #[serde(default = "default_true")]
    pub stream_usage: bool,
}

fn default_true() -> bool {
    true
}

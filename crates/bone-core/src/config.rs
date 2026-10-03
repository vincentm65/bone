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
    /// Run a reply's parallel-safe tool calls at the same time.
    #[serde(default = "default_true")]
    pub parallel_tools: bool,
}

/// A model provider: an OpenAI-compatible `/chat/completions` endpoint, or
/// (with `type`) one written in Lua.
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConfig {
    /// e.g. `https://api.deepseek.com/v1` or `http://localhost:8081/v1`.
    /// Required for the built-in provider; a Lua provider may default it.
    #[serde(default)]
    pub base_url: String,
    /// A provider registered with `bone.provider.register`; `None` for the
    /// built-in OpenAI-compatible one.
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    /// The whole table from `core.lua`, handed to a Lua provider.
    #[serde(skip)]
    pub options: serde_json::Value,
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

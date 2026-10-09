//! Core configuration. Plain data; `scripting::load` builds it from
//! `core.lua` and `BONE_*` overrides.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

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
    /// `bone.config.compact`: summarizing long sessions for the model.
    #[serde(default)]
    pub compact: CompactConfig,
}

/// How long sessions are compacted (see `compact.rs`). Set in `core.lua` as
/// `bone.config.compact`; `settings.json`'s `compact` changes it field by
/// field, and applies at once.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CompactConfig {
    /// The latest user turns always sent word for word.
    pub keep: usize,
    /// Compact and retry when the model says the context is too long.
    pub auto: bool,
    /// Compact before a model call estimated above this many tokens.
    pub limit: Option<u64>,
    /// The `bone.config.providers` entry that writes summaries (default:
    /// the one turns use).
    pub provider: Option<String>,
    /// Instructions for the summary, replacing the built-in ones.
    pub prompt: Option<String>,
}

impl Default for CompactConfig {
    fn default() -> Self {
        CompactConfig {
            keep: 0,
            auto: true,
            limit: None,
            provider: None,
            prompt: None,
        }
    }
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
    /// Replay assistant reasoning as `reasoning_content` for compatible servers.
    /// Opt in explicitly; standard OpenAI requests omit this field.
    #[serde(default)]
    pub replay_reasoning: bool,
}

fn default_true() -> bool {
    true
}

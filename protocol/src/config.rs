//! Daemon-authoritative configuration protocol.

use serde::{Deserialize, Serialize};

fn is_false(value: &bool) -> bool {
    !*value
}

fn default_stream_usage() -> String {
    "auto".to_string()
}

fn is_auto_stream_usage(value: &str) -> bool {
    value == "auto"
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConfigSchema {
    pub pages: Vec<ConfigPage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConfigPage {
    pub namespace: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<SettingDefinition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pages: Vec<ConfigPage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SettingDefinition {
    pub path: String,
    pub key: String,
    pub label: String,
    #[serde(rename = "type")]
    pub value_type: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    pub default: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integer: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    pub reload_behavior: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConfigSnapshot {
    pub revision: u64,
    pub values: serde_json::Value,
    pub providers: Vec<ProviderConfig>,
    pub active_provider: String,
    #[serde(default)]
    pub disabled_tools: Vec<String>,
    #[serde(default)]
    pub disabled_commands: Vec<String>,
}

/// Provider data safe to send to any client. Secrets are represented only by
/// `api_key_configured`; resolved credential values never cross the protocol.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderConfig {
    pub id: String,
    pub label: String,
    pub base_url: String,
    pub model: String,
    pub endpoint: String,
    pub handler: String,
    pub context_window_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrency: Option<usize>,
    pub reasoning_effort: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub fast_mode: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub supports_prompt_cache_key: bool,
    /// `auto` / `true` / `false`; `auto` keeps the built-in host list.
    #[serde(default = "default_stream_usage", skip_serializing_if = "is_auto_stream_usage")]
    pub stream_usage: String,
    pub api_key_configured: bool,
}

/// Provider mutation payload. An omitted key preserves an existing credential;
/// a present key replaces it. Plaintext remains supported during migration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderUpdate {
    pub id: String,
    pub label: String,
    pub base_url: String,
    pub model: String,
    pub endpoint: String,
    pub handler: String,
    pub context_window_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrency: Option<usize>,
    pub reasoning_effort: String,
    /// Omitted updates preserve the provider's current value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast_mode: Option<bool>,
    /// Omitted updates preserve the provider's current value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_prompt_cache_key: Option<bool>,
    /// Omitted updates preserve the provider's current value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_usage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;

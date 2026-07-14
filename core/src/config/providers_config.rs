//! Provider registry shape (`ProvidersConfig` / `ProviderEntry`) parsed from `providers.yaml`.

use serde::{Deserialize, Serialize};

/// A single provider entry. All OpenAI-compatible providers share the same
/// shape; Anthropic-style providers are differentiated by `handler`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProviderEntry {
    /// Human-readable label shown in the status bar.
    #[serde(default, deserialize_with = "string_or_default")]
    pub label: String,

    /// API base URL.
    #[serde(default, deserialize_with = "string_or_default")]
    pub base_url: String,

    /// Model name to send in the request payload.
    #[serde(default, deserialize_with = "string_or_default")]
    pub model: String,

    /// API key (optional for local providers).
    #[serde(default, deserialize_with = "string_or_default")]
    pub api_key: String,

    /// Chat endpoint path (default: /chat/completions).
    #[serde(
        default = "default_endpoint",
        deserialize_with = "string_or_default_endpoint"
    )]
    pub endpoint: String,

    /// Handler style: "openai" (default) or "anthropic".
    #[serde(
        default = "default_handler",
        deserialize_with = "string_or_default_handler"
    )]
    pub handler: String,

    /// Reasoning effort for backends that expose it (Codex Responses
    /// `reasoning.effort`, OpenAI-compatible Chat Completions
    /// `reasoning_effort` for xAI/Grok, etc.). Empty means model default.
    #[serde(default, deserialize_with = "string_or_default")]
    pub reasoning_effort: String,
}

fn string_or_default_with<'de, D>(
    deserializer: D,
    fallback: fn() -> String,
) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    Ok(opt.unwrap_or_else(fallback))
}

fn string_or_default<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    string_or_default_with(deserializer, String::new)
}

fn string_or_default_endpoint<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    string_or_default_with(deserializer, default_endpoint)
}

fn string_or_default_handler<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    string_or_default_with(deserializer, default_handler)
}

fn default_endpoint() -> String {
    "/chat/completions".to_string()
}
fn default_handler() -> String {
    "openai".to_string()
}

impl ProviderEntry {
    /// Non-empty reasoning effort for request builders. Empty/`default` → None.
    pub fn reasoning_effort_opt(&self) -> Option<String> {
        match self.reasoning_effort.trim() {
            "" | "default" => None,
            effort => Some(effort.to_ascii_lowercase()),
        }
    }

    /// Deserialize a ProviderEntry from a nested YAML map value
    /// (as stored in a CustomConfigPage field).
    pub fn from_nested(val: &serde_yaml::Value) -> Option<Self> {
        let map = val.as_mapping()?;
        let get = |key: &str| -> String {
            map.get(key)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        let get_with_default = |key: &str, fallback: &str| -> String {
            map.get(key)
                .and_then(|v| v.as_str())
                .map(|s| {
                    if s.is_empty() {
                        fallback.to_string()
                    } else {
                        s.to_string()
                    }
                })
                .unwrap_or_else(|| fallback.to_string())
        };
        Some(ProviderEntry {
            label: get("label"),
            base_url: get("base_url"),
            model: get("model"),
            api_key: get("api_key"),
            endpoint: get_with_default("endpoint", &default_endpoint()),
            handler: get_with_default("handler", &default_handler()),
            reasoning_effort: get("reasoning_effort"),
        })
    }
}

/// The providers file is a flat map of provider id → config.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ProvidersConfig {
    /// Last used provider id — loaded on app startup.
    #[serde(default)]
    pub last_provider: String,

    #[serde(flatten)]
    pub providers: std::collections::HashMap<String, ProviderEntry>,
}

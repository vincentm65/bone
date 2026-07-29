//! Provider registry shape (`ProvidersConfig` / `ProviderEntry`) parsed from `providers.yaml`.

use serde::{Deserialize, Serialize};

/// A provider credential as written by the user. Exact `${ENV_VAR}` values are
/// resolved only when constructing a runtime provider; all other strings remain
/// plaintext and round-trip unchanged.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderCredential(String);

impl ProviderCredential {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn resolve(&self) -> Result<String, String> {
        let Some(name) = self
            .0
            .strip_prefix("${")
            .and_then(|value| value.strip_suffix('}'))
            .filter(|name| {
                !name.is_empty()
                    && name.chars().enumerate().all(|(index, ch)| {
                        ch == '_' || ch.is_ascii_uppercase() || (index > 0 && ch.is_ascii_digit())
                    })
            })
        else {
            return Ok(self.0.clone());
        };
        std::env::var(name)
            .map_err(|_| format!("provider credential environment variable {name} is not set"))
    }

    pub fn resolve_or_warn(&self) -> String {
        self.resolve().unwrap_or_else(|error| {
            crate::ext::ctx::runtime_warn_once(format!("bone: warning: {error}"));
            String::new()
        })
    }
}

impl From<String> for ProviderCredential {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for ProviderCredential {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl Serialize for ProviderCredential {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ProviderCredential {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Option::<String>::deserialize(deserializer).map(|value| Self(value.unwrap_or_default()))
    }
}

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
    #[serde(default)]
    pub api_key: ProviderCredential,

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

    #[serde(default, deserialize_with = "optional_u64")]
    pub context_window_tokens: Option<u64>,

    /// Maximum delegated agents that may use this provider at once, shared
    /// across Bone processes. Missing values preserve the historical default.
    #[serde(default, deserialize_with = "optional_usize")]
    pub max_concurrency: Option<usize>,

    /// Reasoning effort for backends that expose it (Codex Responses
    /// `reasoning.effort`, OpenAI-compatible Chat Completions
    /// `reasoning_effort` for xAI/Grok, etc.). Empty means model default.
    #[serde(default, deserialize_with = "string_or_default")]
    pub reasoning_effort: String,
}

fn optional_u64<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    optional_number(deserializer)
}

fn optional_usize<'de, D>(deserializer: D) -> Result<Option<usize>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    optional_number(deserializer)
}

fn optional_number<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + std::str::FromStr,
    T::Err: std::fmt::Display,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Value<T> {
        Number(T),
        String(String),
    }
    match Option::<Value<T>>::deserialize(deserializer)? {
        None => Ok(None),
        Some(Value::String(value)) if value.trim().is_empty() => Ok(None),
        Some(Value::Number(value)) => Ok(Some(value)),
        Some(Value::String(value)) => value.parse().map(Some).map_err(serde::de::Error::custom),
    }
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

pub const REASONING_EFFORTS: &[&str] = &[
    "default", "none", "minimal", "low", "medium", "high", "xhigh", "max",
];

pub fn validate_reasoning_effort(value: &str) -> Result<(), String> {
    let value = value.trim();
    if value.is_empty()
        || REASONING_EFFORTS
            .iter()
            .any(|effort| value.eq_ignore_ascii_case(effort))
    {
        Ok(())
    } else {
        Err(format!(
            "unsupported reasoning_effort {value:?}; expected default, none, minimal, low, medium, high, xhigh, or max"
        ))
    }
}

impl ProviderEntry {
    pub const DEFAULT_MAX_CONCURRENCY: usize = 1;

    pub fn max_concurrency(&self) -> usize {
        self.max_concurrency
            .unwrap_or(Self::DEFAULT_MAX_CONCURRENCY)
    }

    /// Non-empty reasoning effort for request builders. Empty/`default` → None.
    pub fn reasoning_effort_opt(&self) -> Option<String> {
        match self.reasoning_effort.trim() {
            "" | "default" => None,
            effort => Some(effort.to_ascii_lowercase()),
        }
    }
}

/// The canonical root `providers.yaml` document.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProvidersConfig {
    #[serde(default = "version_one")]
    pub version: u8,

    /// Last used provider id — loaded on app startup.
    #[serde(default, rename = "active")]
    pub last_provider: String,

    #[serde(default)]
    pub providers: std::collections::HashMap<String, ProviderEntry>,
}

impl Default for ProvidersConfig {
    fn default() -> Self {
        Self {
            version: 1,
            last_provider: String::new(),
            providers: std::collections::HashMap::new(),
        }
    }
}

fn version_one() -> u8 {
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_supported_reasoning_efforts() {
        assert!(validate_reasoning_effort("").is_ok());
        assert!(validate_reasoning_effort("default").is_ok());
        for effort in REASONING_EFFORTS {
            assert!(validate_reasoning_effort(effort).is_ok(), "{effort}");
        }
        assert!(validate_reasoning_effort("HIGH").is_ok());
        assert!(validate_reasoning_effort("extreme").is_err());
    }
}

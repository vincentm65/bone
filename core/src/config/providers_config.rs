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
    /// across Bone processes. Missing values mean unlimited concurrency.
    #[serde(default, deserialize_with = "optional_usize")]
    pub max_concurrency: Option<usize>,

    /// Reasoning effort for backends that expose it (Codex Responses
    /// `reasoning.effort`, OpenAI-compatible Chat Completions
    /// `reasoning_effort` for xAI/Grok, Anthropic `output_config.effort`, etc.).
    /// Empty/`default` means the model default; other values pass through.
    #[serde(default, deserialize_with = "string_or_default")]
    pub reasoning_effort: String,

    /// Request Codex's priority service tier. This is intentionally provider
    /// configuration rather than a generic LLM option: only the Codex handler
    /// reads it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub fast_mode: bool,

    /// Send OpenAI's `prompt_cache_key` request field. Disabled by default so
    /// strict OpenAI-compatible APIs do not receive an unsupported field.
    #[serde(default, skip_serializing_if = "is_false")]
    pub supports_prompt_cache_key: bool,

    /// Request streaming usage (`stream_options.include_usage`). `"auto"`
    /// (default) keeps the built-in host list; `"true"`/`"false"` force the
    /// choice for OpenAI-compatible backends that only emit usage when asked.
    #[serde(
        default = "default_stream_usage",
        deserialize_with = "stream_usage_or_default",
        skip_serializing_if = "is_default_stream_usage"
    )]
    pub stream_usage: String,
}

fn is_false(value: &bool) -> bool {
    !*value
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
fn default_stream_usage() -> String {
    "auto".to_string()
}
fn is_default_stream_usage(value: &str) -> bool {
    value == "auto"
}

/// `stream_usage` accepts the strings `auto`/`true`/`false` (case-insensitive)
/// or a plain boolean, normalizing to lowercase. This is the single canonical
/// domain check shared by the YAML load path and the config write path.
pub(crate) fn normalize_stream_usage(raw: &str) -> Result<String, String> {
    let value = raw.trim().to_ascii_lowercase();
    match value.as_str() {
        "auto" | "true" | "false" => Ok(value),
        other => Err(format!(
            "stream_usage must be \"auto\", \"true\", or \"false\", got {other:?}"
        )),
    }
}

fn stream_usage_or_default<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Bool(bool),
        Text(String),
    }
    match Option::<Raw>::deserialize(deserializer)? {
        None => Ok(default_stream_usage()),
        Some(Raw::Bool(flag)) => Ok(if flag { "true" } else { "false" }.to_string()),
        Some(Raw::Text(text)) => normalize_stream_usage(&text).map_err(serde::de::Error::custom),
    }
}

impl ProviderEntry {
    /// Whether the OpenAI-compatible request should ask the backend for a
    /// streaming usage receipt. `auto` preserves the original host allowlist;
    /// explicit true/false overrides it for that provider.
    pub fn stream_usage_enabled(&self) -> bool {
        match self.stream_usage.as_str() {
            "true" => true,
            "false" => false,
            _ => self.base_url.contains("api.openai.com")
                || self.base_url.contains("api.deepseek.com")
                || self.base_url.contains("cli-chat-proxy.grok.com")
                || self.base_url.contains("127.0.0.1")
                || self.base_url.contains("localhost"),
        }
    }

    /// Non-empty reasoning effort for request builders. Empty/`default` → None.
    pub fn reasoning_effort_opt(&self) -> Option<String> {
        let effort = self.reasoning_effort.trim();
        if effort.is_empty() || effort.eq_ignore_ascii_case("default") {
            return None;
        }
        match effort.to_ascii_lowercase().as_str() {
            "low" => Some("low".into()),
            "medium" => Some("medium".into()),
            "high" => Some("high".into()),
            "xhigh" => Some("xhigh".into()),
            _ => Some(effort.to_string()),
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
#[path = "providers_config_tests.rs"]
mod tests;

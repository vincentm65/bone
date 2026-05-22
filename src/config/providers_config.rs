use serde::{Deserialize, Serialize};

/// A single provider entry. All OpenAI-compatible providers share the same
/// shape; Anthropic-style providers are differentiated by `handler`.
#[derive(Debug, Deserialize, Serialize)]
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
}

fn string_or_default<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    Ok(opt.unwrap_or_default())
}

fn string_or_default_endpoint<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    Ok(opt.unwrap_or_else(default_endpoint))
}

fn string_or_default_handler<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    Ok(opt.unwrap_or_else(default_handler))
}

fn default_endpoint() -> String {
    "/chat/completions".to_string()
}
fn default_handler() -> String {
    "openai".to_string()
}

/// The providers file is a flat map of provider id → config.
#[derive(Debug, Deserialize, Serialize, Default)]
pub struct ProvidersConfig {
    #[serde(flatten)]
    pub providers: std::collections::HashMap<String, ProviderEntry>,
}

pub fn load_providers() -> ProvidersConfig {
    let path = super::paths::providers_path();
    if !path.exists() {
        return ProvidersConfig::default();
    }
    super::load_yaml(&path).unwrap_or_else(|| {
        eprintln!("bone: warning: failed to parse {}", path.display());
        ProvidersConfig::default()
    })
}

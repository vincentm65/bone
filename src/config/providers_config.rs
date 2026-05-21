use serde::{Deserialize, Serialize};

/// A single provider entry. All OpenAI-compatible providers share the same
/// shape; Anthropic-style providers are differentiated by `handler`.
#[derive(Debug, Deserialize, Serialize)]
pub struct ProviderEntry {
    /// Human-readable label shown in the status bar.
    #[serde(default)]
    pub label: String,

    /// API base URL.
    #[serde(default)]
    pub base_url: String,

    /// Model name to send in the request payload.
    #[serde(default)]
    pub model: String,

    /// API key (optional for local providers).
    #[serde(default)]
    pub api_key: String,

    /// Chat endpoint path (default: /chat/completions).
    #[serde(default = "default_endpoint")]
    pub endpoint: String,

    /// Handler style: "openai" (default) or "anthropic".
    #[serde(default = "default_handler")]
    pub handler: String,
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
    super::load_yaml(&path).unwrap_or_default()
}

//! Daemon-authoritative configuration protocol.

use serde::{Deserialize, Serialize};

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
    pub reasoning_effort: String,
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
    pub reasoning_effort: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacted_provider_never_serializes_a_secret_field() {
        let provider = ProviderConfig {
            id: "openai".into(),
            label: "OpenAI".into(),
            base_url: "https://api.openai.com".into(),
            model: "gpt".into(),
            endpoint: "/chat/completions".into(),
            handler: "openai".into(),
            context_window_tokens: None,
            reasoning_effort: String::new(),
            api_key_configured: true,
        };
        let json = serde_json::to_value(provider).unwrap();
        assert!(json.get("api_key").is_none());
        assert_eq!(json["api_key_configured"], true);
    }

    #[test]
    fn omitted_provider_key_preserves_update_intent() {
        let update: ProviderUpdate = serde_json::from_value(serde_json::json!({
            "id": "local",
            "label": "Local",
            "base_url": "http://localhost:8080",
            "model": "model",
            "endpoint": "/chat/completions",
            "handler": "openai",
            "context_window_tokens": null,
            "reasoning_effort": ""
        }))
        .unwrap();
        assert_eq!(update.api_key, None);
    }
}

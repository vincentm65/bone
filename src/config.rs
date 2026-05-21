use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Config paths
// ---------------------------------------------------------------------------

fn bone_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/"))
        .join(".bone-rust")
}

pub fn config_path() -> PathBuf {
    bone_dir().join("bone.yaml")
}

pub fn providers_path() -> PathBuf {
    bone_dir().join("providers.yaml")
}

// ---------------------------------------------------------------------------
// User config (~/.bone-rust/bone.yaml)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Serialize)]
pub struct UserConfig {
    /// Active provider id — must match a key in providers.yaml.
    #[serde(default = "default_provider")]
    pub provider: String,

    /// Context window size in tokens.
    #[serde(default = "default_context_window")]
    pub context_window: usize,

    /// Tokens reserved for the model's response.
    #[serde(default = "default_response_budget")]
    pub response_budget: usize,
}

fn default_provider() -> String {
    "local".to_string()
}
fn default_context_window() -> usize {
    200_000
}
fn default_response_budget() -> usize {
    64_000
}

impl Default for UserConfig {
    fn default() -> Self {
        Self {
            provider: default_provider(),
            context_window: default_context_window(),
            response_budget: default_response_budget(),
        }
    }
}

// ---------------------------------------------------------------------------
// Provider config (~/.bone-rust/providers.yaml)
// ---------------------------------------------------------------------------

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
///
/// Example `~/.bone-rust/providers.yaml`:
/// ```yaml
/// local:
///   label: llama.cpp
///   base_url: http://127.0.0.1:8080
///   model: local
///
/// openrouter:
///   label: OpenRouter
///   base_url: https://openrouter.ai/api/v1
///   model: google/gemini-3.1-flash-lite
///   api_key: sk-or-...
///
/// glm:
///   label: GLM
///   base_url: https://open.bigmodel.cn/api/paas/v4
///   model: GLM-5
///   api_key: your-key
/// ```
#[derive(Debug, Deserialize, Serialize, Default)]
pub struct ProvidersConfig {
    #[serde(flatten)]
    pub providers: std::collections::HashMap<String, ProviderEntry>,
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

pub fn load_user_config() -> UserConfig {
    let path = config_path();
    if !path.exists() {
        return UserConfig::default();
    }
    load_yaml(&path).unwrap_or_default()
}

pub fn load_providers() -> ProvidersConfig {
    let path = providers_path();
    if !path.exists() {
        return ProvidersConfig::default();
    }
    load_yaml(&path).unwrap_or_default()
}

fn load_yaml<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    let raw = fs::read_to_string(path).ok()?;
    // Strip BOM if present
    let raw = raw.trim_start_matches('\u{feff}');
    serde_yaml::from_str(raw).ok()
}

// ---------------------------------------------------------------------------
// Seed defaults
// ---------------------------------------------------------------------------

/// Write a default providers.yaml if one doesn't exist.
pub fn seed_providers_if_missing() {
    let path = providers_path();
    if path.exists() {
        return;
    }
    let default = ProvidersConfig {
        providers: {
            let mut m = std::collections::HashMap::new();
            m.insert(
                "local".to_string(),
                ProviderEntry {
                    label: "llama.cpp".to_string(),
                    base_url: "http://127.0.0.1:8080".to_string(),
                    model: "local".to_string(),
                    api_key: String::new(),
                    endpoint: "/v1/chat/completions".to_string(),
                    handler: "openai".to_string(),
                },
            );
            m
        },
    };
    if let Some(parent) = path.parent()
        && let Err(e) = fs::create_dir_all(parent)
    {
        eprintln!("bone: warning: could not create {}: {e}", parent.display());
        return;
    }
    if let Ok(yaml) = serde_yaml::to_string(&default)
        && let Err(e) = fs::write(&path, yaml)
    {
        eprintln!("bone: warning: could not write {}: {e}", path.display());
    }
}

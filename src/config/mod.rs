pub mod providers_config;

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use providers_config::{ProviderEntry, ProvidersConfig, load_providers, save_providers};

// ── shared YAML loader ──────────────────────────────────────────────────────

pub(crate) fn load_yaml<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    let raw = std::fs::read_to_string(path).ok()?;
    let raw = raw.trim_start_matches('\u{feff}');
    serde_yaml::from_str(raw).ok()
}

// ── paths ───────────────────────────────────────────────────────────────────

fn bone_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("bone-rust");
    }
    std::env::var("HOME")
        .ok()
        .map(|home| PathBuf::from(home).join(".bone-rust"))
        .unwrap_or_else(|| {
            eprintln!(
                "bone: warning: neither $HOME nor $XDG_CONFIG_HOME is set; using /tmp/.bone-rust"
            );
            PathBuf::from("/tmp/.bone-rust")
        })
}

pub fn config_path() -> PathBuf {
    bone_dir().join("bone.yaml")
}

pub fn providers_path() -> PathBuf {
    bone_dir().join("providers.yaml")
}

// ── UserConfig ──────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Serialize)]
pub struct UserConfig {
    #[serde(default = "default_provider")]
    pub provider: String,
}

fn default_provider() -> String {
    "local".to_string()
}

impl Default for UserConfig {
    fn default() -> Self {
        Self {
            provider: default_provider(),
        }
    }
}

pub fn load_user_config() -> UserConfig {
    let path = config_path();
    if !path.exists() {
        return UserConfig::default();
    }
    load_yaml(&path).unwrap_or_else(|| {
        eprintln!("bone: warning: failed to parse {}", path.display());
        UserConfig::default()
    })
}

// ── seed providers ──────────────────────────────────────────────────────────

pub fn seed_providers_if_missing() {
    let path = providers_path();
    if path.exists() {
        return;
    }
    let default = ProvidersConfig {
        last_provider: String::new(),
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

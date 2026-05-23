use std::fs;

use super::paths::providers_path;
use super::providers_config::{ProviderEntry, ProvidersConfig};

/// Write a default providers.yaml if one doesn't exist.
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

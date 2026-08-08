//! Canonical peer configuration documents outside `config.yaml`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::bone_dir;
use super::settings::{ExtensionValue, SubagentSettings};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SubagentsConfig {
    #[serde(default = "version_one")]
    pub version: u8,
    #[serde(default)]
    pub subagents: BTreeMap<String, SubagentSettings>,
}

impl Default for SubagentsConfig {
    fn default() -> Self {
        Self {
            version: 1,
            subagents: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionsConfig {
    #[serde(default = "version_one")]
    pub version: u8,
    #[serde(default)]
    pub extensions: BTreeMap<String, BTreeMap<String, ExtensionValue>>,
}

impl Default for ExtensionsConfig {
    fn default() -> Self {
        Self {
            version: 1,
            extensions: BTreeMap::new(),
        }
    }
}

fn version_one() -> u8 {
    1
}

pub fn subagents_path() -> PathBuf {
    bone_dir().join("subagents.yaml")
}

pub fn extensions_path() -> PathBuf {
    bone_dir().join("extensions.yaml")
}

pub fn load_providers() -> Result<Option<super::ProvidersConfig>, String> {
    let path = super::providers_path();
    let loaded: Option<super::ProvidersConfig> = load_versioned(&path)?;
    if let Some(config) = &loaded {
        validate_providers(config)?;
    }
    Ok(loaded)
}

pub fn load_or_seed_providers() -> Result<super::ProvidersConfig, String> {
    if let Some(config) = load_providers()? {
        return Ok(config);
    }
    let mut config = super::ProvidersConfig::default();
    for (id, label, base_url, model, endpoint, handler) in [
        (
            "local",
            "llama.cpp",
            "http://127.0.0.1:8080",
            "local",
            "/v1/chat/completions",
            "openai",
        ),
        (
            "openai",
            "OpenAI",
            "https://api.openai.com/v1",
            "gpt-4o",
            "/chat/completions",
            "openai",
        ),
        (
            "anthropic",
            "Anthropic",
            "https://api.anthropic.com",
            "claude-sonnet-4-20250514",
            "/messages",
            "anthropic",
        ),
    ] {
        config.providers.insert(
            id.into(),
            super::ProviderEntry {
                label: label.into(),
                base_url: base_url.into(),
                model: model.into(),
                api_key: super::ProviderCredential::default(),
                endpoint: endpoint.into(),
                handler: handler.into(),
                context_window_tokens: None,
                max_concurrency: None,
                reasoning_effort: String::new(),
                fast_mode: false,
                supports_prompt_cache_key: id == "openai",
            },
        );
    }
    validate_providers(&config)?;
    persist_providers(&config)?;
    Ok(config)
}

pub(crate) fn validate_providers(config: &super::ProvidersConfig) -> Result<(), String> {
    if config.version != 1 {
        return Err(format!(
            "unsupported version {} in {}; expected 1",
            config.version,
            super::providers_path().display()
        ));
    }
    if !config.last_provider.is_empty() && !config.providers.contains_key(&config.last_provider) {
        return Err(format!(
            "active provider {:?} is not defined in {}",
            config.last_provider,
            super::providers_path().display()
        ));
    }
    for (id, provider) in &config.providers {
        if provider.max_concurrency == Some(0) {
            return Err(format!("providers.{id}.max_concurrency must be at least 1"));
        }
        if provider.fast_mode && provider.handler != "codex" {
            return Err(format!(
                "providers.{id}.fast_mode is only supported by the codex handler"
            ));
        }
    }
    Ok(())
}

pub fn load_subagents() -> Result<Option<SubagentsConfig>, String> {
    let path = subagents_path();
    let loaded: Option<SubagentsConfig> = load_versioned(&path)?;
    if let Some(config) = &loaded {
        validate_version(config.version, &path)?;
        super::settings::validate_subagents(&config.subagents)
            .map_err(|error| error.to_string())?;
    }
    Ok(loaded)
}

pub fn load_or_seed_subagents() -> Result<SubagentsConfig, String> {
    if let Some(config) = load_subagents()? {
        return Ok(config);
    }
    let config = SubagentsConfig::default();
    persist_subagents(&config.subagents)?;
    Ok(config)
}

pub fn load_extensions() -> Result<Option<ExtensionsConfig>, String> {
    let path = extensions_path();
    let loaded: Option<ExtensionsConfig> = load_versioned(&path)?;
    if let Some(config) = &loaded {
        validate_version(config.version, &path)?;
    }
    Ok(loaded)
}

pub fn load_or_seed_extensions() -> Result<ExtensionsConfig, String> {
    if let Some(config) = load_extensions()? {
        return Ok(config);
    }
    let config = ExtensionsConfig::default();
    persist_extensions(&config.extensions)?;
    Ok(config)
}

fn validate_version(version: u8, path: &Path) -> Result<(), String> {
    if version == 1 {
        Ok(())
    } else {
        Err(format!(
            "unsupported version {version} in {}; expected 1",
            path.display()
        ))
    }
}

fn load_versioned<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>, String> {
    if !path.exists() {
        return Ok(None);
    }
    super::load_yaml(path).map(Some)
}

pub fn persist_providers(values: &super::ProvidersConfig) -> Result<(), String> {
    validate_providers(values)?;
    write_document(&super::providers_path(), values, None)
}

pub fn persist_subagents(values: &BTreeMap<String, SubagentSettings>) -> Result<(), String> {
    super::settings::validate_subagents(values).map_err(|error| error.to_string())?;
    write_document(
        &subagents_path(),
        &SubagentsConfig {
            version: 1,
            subagents: values.clone(),
        },
        None,
    )
}

pub fn persist_extensions(
    values: &BTreeMap<String, BTreeMap<String, ExtensionValue>>,
) -> Result<(), String> {
    write_document(
        &extensions_path(),
        &ExtensionsConfig {
            version: 1,
            extensions: values.clone(),
        },
        None,
    )
}

pub(crate) fn write_document<T: Serialize>(
    path: &Path,
    value: &T,
    permissions_from: Option<&Path>,
) -> Result<(), String> {
    let yaml = serde_yaml::to_string(value).map_err(|error| error.to_string())?;
    write_bytes(path, yaml.as_bytes(), permissions_from)
}

pub(crate) fn write_bytes(
    path: &Path,
    content: &[u8],
    permissions_from: Option<&Path>,
) -> Result<(), String> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let permissions = std::fs::metadata(path)
        .ok()
        .or_else(|| permissions_from.and_then(|path| std::fs::metadata(path).ok()))
        .map(|metadata| metadata.permissions());
    #[cfg(unix)]
    let permissions = permissions.or_else(|| {
        use std::os::unix::fs::PermissionsExt;
        Some(std::fs::Permissions::from_mode(0o600))
    });
    crate::tools::write_atomic::write_atomic_sync(path, content, permissions)
}

#[cfg(test)]
#[path = "domains_tests.rs"]
mod tests;

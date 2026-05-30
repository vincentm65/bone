pub mod providers_config;

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::tools::ApprovalMode;
pub use providers_config::{ProviderEntry, ProvidersConfig, load_providers, save_providers};

pub(crate) fn load_yaml<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    let raw = std::fs::read_to_string(path).ok()?;
    let raw = raw.trim_start_matches('\u{feff}');
    serde_yaml::from_str(raw).ok()
}

pub(crate) fn bone_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("bone-rust");
    }
    if let Ok(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
        return PathBuf::from(home).join(".bone-rust");
    }
    eprintln!(
        "bone: warning: neither $HOME, $USERPROFILE nor $XDG_CONFIG_HOME is set; using /tmp/.bone-rust"
    );
    PathBuf::from("/tmp/.bone-rust")
}

pub fn config_path() -> PathBuf {
    bone_dir().join("bone.yaml")
}

pub fn providers_path() -> PathBuf {
    bone_dir().join("providers.yaml")
}

pub fn command_policy_path() -> PathBuf {
    bone_dir().join("command-policy.yaml")
}

pub fn skills_dir() -> PathBuf {
    bone_dir().join("skills")
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UserConfig {
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default)]
    pub approval_mode: ApprovalMode,
    #[serde(default = "default_enabled_tools")]
    pub enabled_tools: Vec<String>,
    #[serde(default = "default_max_rounds")]
    pub max_rounds: u32,
    #[serde(default)]
    pub auto_compact_tokens: Option<u64>,
    #[serde(default)]
    pub auto_compact_keep_messages: Option<usize>,
}

fn default_provider() -> String {
    "local".to_string()
}

fn default_max_rounds() -> u32 {
    150
}

pub fn default_enabled_tools() -> Vec<String> {
    [
        "read_file",
        "write_file",
        "edit_file",
        "shell",
        "web_search",
        "task_list",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

impl Default for UserConfig {
    fn default() -> Self {
        Self {
            provider: default_provider(),
            approval_mode: ApprovalMode::default(),
            enabled_tools: default_enabled_tools(),
            max_rounds: default_max_rounds(),
            auto_compact_tokens: None,
            auto_compact_keep_messages: None,
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

pub fn save_user_config(config: &UserConfig) {
    let path = config_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(yaml) = serde_yaml::to_string(config) {
        let _ = fs::write(path, yaml);
    }
}

const EXAMPLE_PROVIDERS: &str = include_str!("../../example-providers.yaml");
const DEFAULT_COMMAND_POLICY: &str = include_str!("../../default-command-policy.yaml");

pub fn seed_providers_if_missing() {
    let path = providers_path();
    seed_file_if_missing(&path, EXAMPLE_PROVIDERS);
}

pub fn seed_command_policy_if_missing() {
    let path = command_policy_path();
    seed_file_if_missing(&path, DEFAULT_COMMAND_POLICY);
}

fn seed_file_if_missing(path: &Path, content: &str) {
    if path.exists() {
        return;
    }
    if let Some(parent) = path.parent()
        && let Err(e) = fs::create_dir_all(parent)
    {
        eprintln!("bone: warning: could not create {}: {e}", parent.display());
        return;
    }
    if let Err(e) = fs::write(path, content) {
        eprintln!("bone: warning: could not write {}: {e}", path.display());
    }
}

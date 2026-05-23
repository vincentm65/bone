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

pub fn command_policy_path() -> PathBuf {
    bone_dir().join("command-policy.yaml")
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
    if let Some(parent) = path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            eprintln!("bone: warning: could not create {}: {e}", parent.display());
            return;
        }
    }
    if let Err(e) = fs::write(path, content) {
        eprintln!("bone: warning: could not write {}: {e}", path.display());
    }
}

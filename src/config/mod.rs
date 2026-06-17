pub mod custom;
pub mod providers_config;

use std::fs;
use std::path::{Path, PathBuf};

use crate::ext;
use crate::tools::ApprovalMode;
pub use providers_config::{ProviderEntry, ProvidersConfig};

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

pub fn providers_path() -> PathBuf {
    bone_dir().join("config/providers.yaml")
}

pub fn command_policy_path() -> PathBuf {
    bone_dir().join("command-policy.yaml")
}

/// Runtime configuration populated from config/*.yaml pages.
/// No longer persisted to a single file — all values come from CustomConfigs.
#[derive(Debug, Clone)]
pub struct UserConfig {
    pub approval_mode: ApprovalMode,
    pub enabled_tools: Vec<String>,
    pub status_show: std::collections::HashMap<String, bool>,
}

pub fn default_enabled_tools() -> Vec<String> {
    ["read_file", "write_file", "edit_file", "shell"]
        .into_iter()
        .map(String::from)
        .collect()
}

impl Default for UserConfig {
    fn default() -> Self {
        Self {
            approval_mode: ApprovalMode::default(),
            enabled_tools: default_enabled_tools(),
            status_show: Self::default_status_show(),
        }
    }
}

fn bool_config(custom: &custom::CustomConfigs, key: &str) -> bool {
    custom.get_value("status", key).parse().unwrap_or(true)
}
impl UserConfig {
    pub(crate) const STATUS_TOGGLE_KEYS: [&'static str; 9] = [
        "status_show_model",
        "status_show_approval",
        "status_show_tokens_curr",
        "status_show_tokens_in",
        "status_show_tokens_out",
        "status_show_tokens_total",
        "status_show_queue",
        "status_show_spinner",
        "status_show_timer",
    ];

    fn default_status_show() -> std::collections::HashMap<String, bool> {
        Self::STATUS_TOGGLE_KEYS
            .iter()
            .map(|&k| (k.to_string(), true))
            .collect()
    }

    /// Helper to check a status_show toggle. Returns true if missing (default on).
    pub fn status_show(&self, key: &str) -> bool {
        self.status_show.get(key).copied().unwrap_or(true)
    }

    /// Build a UserConfig by reading all values from the custom config pages.
    pub fn from_custom_configs(custom: &custom::CustomConfigs) -> Self {
        let mut cfg = Self::default();
        cfg.apply_custom_configs(custom);
        cfg
    }

    /// Populate fields from the custom config pages.
    pub fn apply_custom_configs(&mut self, custom: &custom::CustomConfigs) {
        // General page
        self.approval_mode = match custom.get_value("general", "approval_mode").as_str() {
            "danger" => ApprovalMode::Danger,
            _ => ApprovalMode::Safe,
        };
        self.enabled_tools = custom.enabled_tool_names();
        if self.enabled_tools.is_empty() {
            self.enabled_tools = default_enabled_tools();
        }

        // Status bar toggles
        for key in Self::STATUS_TOGGLE_KEYS {
            if let Some(val) = self.status_show.get_mut(key) {
                *val = bool_config(custom, key);
            }
        }
    }
}

const DEFAULT_COMMAND_POLICY: &str = include_str!("../../default-command-policy.yaml");
const DEFAULT_AGENTS_MD: &str = include_str!("../../defaults/AGENTS.md");

pub fn seed_command_policy_if_missing() {
    let path = command_policy_path();
    seed_file_if_missing(&path, DEFAULT_COMMAND_POLICY);
}

pub fn seed_agents_md_if_missing() {
    let path = bone_dir().join("AGENTS.md");
    seed_file_if_missing(&path, DEFAULT_AGENTS_MD);
}

pub fn seed_file_if_missing(path: &Path, content: &str) {
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
/// Seed all file-based config if missing. Should be called once at startup
/// from every entry point (TUI, run, agent).
pub fn seed_all() {
    seed_command_policy_if_missing();
    seed_agents_md_if_missing();
    custom::seed_builtin_pages();
    ext::seed_default_lua_tools(&bone_dir().join("lua/tools"));
    ext::seed_default_lua_libs(&bone_dir().join("lua/lib"));
}

fn is_local_base_url(base_url: &str) -> bool {
    let host_port = base_url
        .split_once("://")
        .map_or(base_url, |(_, rest)| rest)
        .split('/')
        .next()
        .unwrap_or("");
    let host = if let Some(rest) = host_port.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        host_port.split(':').next().unwrap_or("")
    };
    host.eq_ignore_ascii_case("localhost") || matches!(host, "127.0.0.1" | "::1")
}

fn has_codex_auth_token() -> bool {
    let path = bone_dir()
        .parent()
        .map_or_else(dirs::home_dir, |p| Some(p.to_path_buf()))
        .unwrap_or_default()
        .join(".codex/auth.json");
    let Ok(data) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(doc): Result<serde_json::Value, _> = serde_json::from_str(&data) else {
        return false;
    };
    doc["tokens"]["access_token"]
        .as_str()
        .is_some_and(|s| !s.is_empty())
}

/// Check if a provider has an API key configured. Print a helpful warning
/// if not, so new users know what to do next.
pub fn warn_if_no_api_key_for(provider_id: &str, config: &ProvidersConfig) {
    let Some(entry) = config.providers.get(provider_id) else {
        eprintln!(
            "bone: warning: provider '{}' not found in {}",
            provider_id,
            providers_path().display()
        );
        return;
    };

    if !entry.api_key.is_empty()
        || is_local_base_url(&entry.base_url)
        || (entry.handler == "codex" && has_codex_auth_token())
    {
        return;
    }
    eprintln!(
        "bone: warning: provider '{}' has no API key configured.",
        provider_id
    );
    eprintln!(
        "  Edit {} and add your API key.",
        providers_path().display()
    );
}

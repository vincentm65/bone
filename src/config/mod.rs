pub mod custom;
pub mod providers_config;

use std::fs;
use std::path::{Path, PathBuf};

use crate::tools::{ApprovalMode, default_dynamic_tool_names};
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

pub fn providers_path() -> PathBuf {
    bone_dir().join("providers.yaml")
}

pub fn command_policy_path() -> PathBuf {
    bone_dir().join("command-policy.yaml")
}

pub fn skills_dir() -> PathBuf {
    bone_dir().join("skills")
}

/// Runtime configuration populated from config/*.yaml pages.
/// No longer persisted to a single file — all values come from CustomConfigs.
#[derive(Debug, Clone)]
pub struct UserConfig {
    pub approval_mode: ApprovalMode,
    pub enabled_tools: Vec<String>,
    pub max_rounds: u32,
    pub auto_compact_tokens: Option<u64>,
    pub auto_compact_keep_messages: Option<usize>,
    pub subagent: SubagentConfig,
}

fn default_max_rounds() -> u32 {
    150
}

pub fn default_enabled_tools() -> Vec<String> {
    let mut tools = ["read_file", "write_file", "edit_file", "shell"]
        .into_iter()
        .map(String::from)
        .collect::<Vec<_>>();
    tools.extend(default_dynamic_tool_names().into_iter().map(String::from));
    tools
}

impl Default for UserConfig {
    fn default() -> Self {
        Self {
            approval_mode: ApprovalMode::default(),
            enabled_tools: default_enabled_tools(),
            max_rounds: default_max_rounds(),
            auto_compact_tokens: None,
            auto_compact_keep_messages: None,
            subagent: SubagentConfig::default(),
        }
    }
}

impl UserConfig {
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
            "edit" => ApprovalMode::Edits,
            _ => ApprovalMode::Safe,
        };
        self.enabled_tools = custom.enabled_tool_names();
        if self.enabled_tools.is_empty() {
            self.enabled_tools = default_enabled_tools();
        }
        self.max_rounds = custom
            .get_value("general", "max_rounds")
            .parse()
            .unwrap_or(150);
        self.auto_compact_tokens = {
            let v = custom.get_value("general", "auto_compact_tokens");
            if v.is_empty() { None } else { v.parse().ok() }
        };
        self.auto_compact_keep_messages = {
            let v = custom.get_value("general", "auto_compact_keep_messages");
            if v.is_empty() { None } else { v.parse().ok() }
        };

        // Subagent page
        self.subagent.provider = custom.get_value("subagent", "provider");
        self.subagent.model = custom.get_value("subagent", "model");
        self.subagent.approval = match custom.get_value("subagent", "approval").as_str() {
            "danger" => ApprovalMode::Danger,
            "edit" => ApprovalMode::Edits,
            _ => ApprovalMode::Safe,
        };
        self.subagent.max_rounds = custom
            .get_value("subagent", "max_rounds")
            .parse()
            .unwrap_or(150);
    }
}

#[derive(Debug, Clone)]
pub struct SubagentConfig {
    pub provider: String,
    pub model: String,
    pub approval: ApprovalMode,
    pub max_rounds: u32,
}

impl Default for SubagentConfig {
    fn default() -> Self {
        Self {
            provider: "local".to_string(),
            model: "local".to_string(),
            approval: ApprovalMode::Safe,
            max_rounds: 150,
        }
    }
}

const EXAMPLE_PROVIDERS: &str = include_str!("../../example-providers.yaml");
const DEFAULT_COMMAND_POLICY: &str = include_str!("../../default-command-policy.yaml");
const DEFAULT_AGENTS_MD: &str = include_str!("../../defaults/AGENTS.md");

pub fn seed_providers_if_missing() {
    // Repair the short-lived config/providers.yaml location if a previous build
    // created it. Providers are intentionally not config pages.
    let root_path = providers_path();
    let config_path = custom::config_dir().join("providers.yaml");
    if config_path.exists() && !root_path.exists() {
        if let Some(parent) = root_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Err(e) = fs::rename(&config_path, &root_path) {
            eprintln!(
                "bone: warning: could not move {} to {}: {e}",
                config_path.display(),
                root_path.display()
            );
        }
    }
    seed_file_if_missing(&root_path, EXAMPLE_PROVIDERS);
}

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

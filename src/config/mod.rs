pub mod custom;
pub mod providers_config;

use std::fs;
use std::path::{Path, PathBuf};

use crate::skills;
use crate::tools;
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
    bone_dir().join("config/providers.yaml")
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
    pub auto_compact_tokens: Option<u64>,
    pub auto_compact_keep_messages: Option<usize>,
    pub status_show_model: bool,
    pub status_show_approval: bool,
    pub status_show_tokens_curr: bool,
    pub status_show_tokens_in: bool,
    pub status_show_tokens_out: bool,
    pub status_show_tokens_total: bool,
    pub status_show_tps: bool,
    pub status_show_queue: bool,
    pub status_show_spinner: bool,
    pub status_show_timer: bool,
    pub subagent: SubagentConfig,
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
            auto_compact_tokens: None,
            auto_compact_keep_messages: None,
            status_show_model: true,
            status_show_approval: true,
            status_show_tokens_curr: true,
            status_show_tokens_in: true,
            status_show_tokens_out: true,
            status_show_tokens_total: true,
            status_show_tps: true,
            status_show_queue: true,
            status_show_spinner: true,
            status_show_timer: true,
            subagent: SubagentConfig::default(),
        }
    }
}

fn bool_config(custom: &custom::CustomConfigs, key: &str) -> bool {
    custom.get_value("general", key).parse().unwrap_or(true)
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
        self.auto_compact_tokens = {
            let v = custom.get_value("general", "auto_compact_tokens");
            if v.is_empty() { None } else { v.parse().ok() }
        };
        self.auto_compact_keep_messages = {
            let v = custom.get_value("general", "auto_compact_keep_messages");
            if v.is_empty() { None } else { v.parse().ok() }
        };

        // Status bar toggles
        // Status bar toggles
        self.status_show_model = bool_config(custom, "status_show_model");
        self.status_show_approval = bool_config(custom, "status_show_approval");
        self.status_show_tokens_curr = bool_config(custom, "status_show_tokens_curr");
        self.status_show_tokens_in = bool_config(custom, "status_show_tokens_in");
        self.status_show_tokens_out = bool_config(custom, "status_show_tokens_out");
        self.status_show_tokens_total = bool_config(custom, "status_show_tokens_total");
        self.status_show_tps = bool_config(custom, "status_show_tps");
        self.status_show_queue = bool_config(custom, "status_show_queue");
        self.status_show_spinner = bool_config(custom, "status_show_spinner");
        self.status_show_timer = bool_config(custom, "status_show_timer");
        self.subagent.provider = custom.get_value("subagent", "provider");
        self.subagent.model = custom.get_value("subagent", "model");
        self.subagent.approval = match custom.get_value("subagent", "approval").as_str() {
            "danger" => ApprovalMode::Danger,
            "edit" => ApprovalMode::Edits,
            _ => ApprovalMode::Safe,
        };
    }
}

#[derive(Debug, Clone)]
pub struct SubagentConfig {
    pub provider: String,
    pub model: String,
    pub approval: ApprovalMode,
}

impl Default for SubagentConfig {
    fn default() -> Self {
        Self {
            provider: "local".to_string(),
            model: "local".to_string(),
            approval: ApprovalMode::Safe,
        }
    }
}

const EXAMPLE_PROVIDERS: &str = include_str!("../../defaults/providers.yaml");
const DEFAULT_COMMAND_POLICY: &str = include_str!("../../default-command-policy.yaml");
const DEFAULT_AGENTS_MD: &str = include_str!("../../defaults/AGENTS.md");

pub fn seed_providers_if_missing() {
    seed_file_if_missing(&providers_path(), EXAMPLE_PROVIDERS);
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
/// Seed all file-based config if missing. Should be called once at startup
/// from every entry point (TUI, run, agent).
pub fn seed_all() {
    seed_providers_if_missing();
    seed_command_policy_if_missing();
    seed_agents_md_if_missing();
    custom::seed_builtin_pages();
    if let Err(e) = skills::seed_example_skills() {
        eprintln!("bone: warning: could not seed skills: {e}");
    }
    tools::seed_default_tools(&tools::tools_dir());
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
    doc["tokens"]["access_token"].as_str().is_some_and(|s| !s.is_empty())
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

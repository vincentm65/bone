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
    /// Stream model reasoning/thinking into a live bottom pane while a turn
    /// runs. Off by default; reasoning is otherwise dropped (only the spinner
    /// shows). See `RuntimeEvent::ReasoningDelta` handling in the stream pump.
    pub show_thinking: bool,
    /// Spinner style presets (frames + speed) snapshotted from ui.spinners.
    pub spinner_styles: Vec<crate::ext::snapshots::SpinnerPreset>,
    /// Rotating thinking-text presets snapshotted from ui.spinners.
    pub spinner_texts: Vec<crate::ext::snapshots::TextPreset>,
    /// Selected spinner style name (status_spinner_style).
    pub spinner_style: String,
    /// Selected thinking-text preset name (status_spinner_text).
    pub spinner_text: String,
    /// Spinner speed override in ms/frame; 0 means use the style's own speed.
    pub spinner_speed: u64,
    /// Rotate thinking-text phrases while streaming.
    pub spinner_text_rotate: bool,
    /// Thinking-text rotation speed in ms/phrase; 0 means one phrase per spinner cycle.
    pub spinner_text_speed: u64,
    /// Comma-separated custom thinking-text phrases. Non-empty overrides preset phrases.
    pub spinner_text_custom: String,
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
            show_thinking: false,
            spinner_styles: Vec::new(),
            spinner_texts: Vec::new(),
            spinner_style: "braille".to_string(),
            spinner_text: "thinking".to_string(),
            spinner_speed: 0,
            spinner_text_rotate: true,
            spinner_text_speed: 0,
            spinner_text_custom: String::new(),
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
        self.show_thinking = custom.get_value("general", "show_thinking") == "true";

        // Status bar toggles
        for key in Self::STATUS_TOGGLE_KEYS {
            if let Some(val) = self.status_show.get_mut(key) {
                *val = bool_config(custom, key);
            }
        }

        // Spinner selection (status page)
        let style = custom.get_value("status", "status_spinner_style");
        self.spinner_style = if style.is_empty() {
            "braille".to_string()
        } else {
            style
        };
        let text = custom.get_value("status", "status_spinner_text");
        self.spinner_text = if text.is_empty() {
            "thinking".to_string()
        } else {
            text
        };
        let speed = custom.get_value("status", "status_spinner_speed");
        self.spinner_speed = speed.parse::<u64>().ok().filter(|&v| v > 0).unwrap_or(0);
        self.spinner_text_rotate =
            custom.get_value("status", "status_spinner_text_rotate") != "false";
        let text_speed = custom.get_value("status", "status_spinner_text_speed");
        self.spinner_text_speed = text_speed.parse::<u64>().ok().unwrap_or(0);
        self.spinner_text_custom = custom.get_value("status", "status_spinner_text_custom");
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
    seed_file_forced(path, content);
}

/// Write `content` to `path`, overwriting any existing file. Used by the
/// /setup re-seed action to refresh bundled files with this build's defaults.
pub fn seed_file_forced(path: &Path, content: &str) {
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
/// The onboarding wizard's persisted choices: which bundled tools/commands the
/// user opted into. Doubles as the "already onboarded" marker — its presence
/// means setup has run. Absent it, seeding falls back to "seed everything".
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SetupSelection {
    /// Chosen tool filenames, e.g. `["subagent.lua", "web_search.lua"]`.
    pub tools: Vec<String>,
    /// Chosen command filenames, e.g. `["compact.lua", "memory.lua"]`.
    pub commands: Vec<String>,
}

impl SetupSelection {
    /// The selected tool filenames as a lookup set.
    pub fn tool_set(&self) -> std::collections::HashSet<String> {
        self.tools.iter().cloned().collect()
    }

    /// The selected command filenames as a lookup set.
    pub fn command_set(&self) -> std::collections::HashSet<String> {
        self.commands.iter().cloned().collect()
    }
}

pub fn setup_selection_path() -> PathBuf {
    bone_dir().join(".setup.json")
}

/// Load the persisted onboarding selection, if the user has run setup.
pub fn load_setup_selection() -> Option<SetupSelection> {
    let data = fs::read_to_string(setup_selection_path()).ok()?;
    serde_json::from_str(&data).ok()
}

/// Persist the onboarding selection (also marks onboarding complete).
pub fn save_setup_selection(selection: &SetupSelection) -> std::io::Result<()> {
    let path = setup_selection_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(selection)
        .unwrap_or_else(|_| "{\"tools\":[],\"commands\":[]}".to_string());
    fs::write(path, json)
}

/// True only for a genuinely fresh install: no `init.lua` and no setup marker.
/// Existing users upgrading (who already have an `init.lua`) are never forced
/// through the wizard.
pub fn needs_onboarding() -> bool {
    !bone_dir().join("init.lua").exists() && !setup_selection_path().exists()
}

/// Seed the always-safe, selection-independent config (command policy, AGENTS,
/// built-in config pages, Lua libraries). Idempotent.
pub fn seed_base() {
    seed_command_policy_if_missing();
    seed_agents_md_if_missing();
    custom::seed_builtin_pages(None, false);
    ext::seed_default_lua_libs(&bone_dir().join("lua/lib"), None, false);
}

/// Seed all file-based config if missing. Should be called once at startup
/// from every entry point (TUI, run, agent).
pub fn seed_all() {
    seed_all_with(None);
}

/// Seed base config plus default tools, filtered by the onboarding selection.
/// `None` seeds every bundled tool (default / upgrade behavior).
pub fn seed_all_with(selection: Option<&SetupSelection>) {
    seed_base();
    let allow = selection.map(SetupSelection::tool_set);
    ext::seed_default_lua_tools(&bone_dir().join("lua/tools"), allow.as_ref(), false);
}

/// The bundled files the /setup re-seed checklist can refresh, grouped by
/// category as `(filename, description)`. Tools and commands are limited to the
/// persisted onboarding selection — the set a re-seed would otherwise touch;
/// libraries and config pages list everything bundled. `init.lua`, `AGENTS.md`,
/// and `command-policy.yaml` are intentionally absent.
pub struct ReseedCatalog {
    pub config_pages: Vec<(&'static str, String)>,
    pub libs: Vec<(&'static str, String)>,
    pub tools: Vec<(&'static str, String)>,
    pub commands: Vec<(&'static str, String)>,
}

pub fn reseed_catalog() -> ReseedCatalog {
    let selection = load_setup_selection();
    let filter = |catalog: Vec<(&'static str, String)>,
                  allow: Option<std::collections::HashSet<String>>| {
        catalog
            .into_iter()
            .filter(|(name, _)| allow.as_ref().is_none_or(|a| a.contains(*name)))
            .collect::<Vec<_>>()
    };
    ReseedCatalog {
        config_pages: custom::builtin_page_catalog(),
        libs: ext::default_lib_catalog(),
        tools: filter(
            ext::default_tool_catalog(),
            selection.as_ref().map(SetupSelection::tool_set),
        ),
        commands: filter(
            ext::default_command_catalog(),
            selection.as_ref().map(SetupSelection::command_set),
        ),
    }
}

/// Force-overwrite the chosen bundled files with this build's versions. Each
/// set names the files (by filename, as in [`reseed_catalog`]) to refresh in
/// its category; files absent from a set are left untouched. Does NOT touch
/// init.lua, AGENTS.md, or command-policy. Backs the /setup re-seed checklist.
pub fn reseed_selected(
    config_pages: &std::collections::HashSet<String>,
    libs: &std::collections::HashSet<String>,
    tools: &std::collections::HashSet<String>,
    commands: &std::collections::HashSet<String>,
) -> std::io::Result<()> {
    custom::seed_builtin_pages(Some(config_pages), true);
    ext::seed_default_lua_libs(&bone_dir().join("lua/lib"), Some(libs), true);
    ext::seed_default_lua_tools(&bone_dir().join("lua/tools"), Some(tools), true);
    ext::seed_default_lua_commands(&bone_dir().join("lua/commands"), Some(commands), true);
    Ok(())
}

/// Seed using whatever selection is persisted on disk (or all, if none).
pub fn seed_all_with_persisted() {
    seed_all_with(load_setup_selection().as_ref());
}

/// The user's `init.lua` choice in the onboarding wizard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitChoice {
    /// Banner + a live sub-agent.
    Populated,
    /// Minimal placeholder.
    Blank,
    /// Leave the existing `init.lua` untouched (offered only when one exists).
    Keep,
}

/// Persist the wizard's results and materialize them on disk: the selection
/// file (also the onboarding marker), the chosen `init.lua`, and the seeded
/// tools/commands filtered to the selection.
pub fn apply_onboarding(selection: &SetupSelection, init: InitChoice) -> std::io::Result<()> {
    // Materialize everything first; only write the selection file (the
    // "onboarding complete" marker) last, so a failure partway through leaves
    // `needs_onboarding()` true and the wizard runs again next launch.
    let init_path = bone_dir().join("init.lua");
    let content = match init {
        InitChoice::Populated => Some(ext::populated_init_lua()),
        InitChoice::Blank => Some(ext::blank_init_lua()),
        InitChoice::Keep => None,
    };
    if let Some(content) = content {
        if let Some(parent) = init_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&init_path, content)?;
    }

    seed_base();
    ext::seed_default_lua_tools(&bone_dir().join("lua/tools"), Some(&selection.tool_set()), false);
    ext::seed_default_lua_commands(
        &bone_dir().join("lua/commands"),
        Some(&selection.command_set()),
        false,
    );

    save_setup_selection(selection)
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

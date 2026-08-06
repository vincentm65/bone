//! User configuration loading: canonical YAML domains and provider entries.

pub mod domains;
pub mod error;
pub mod providers_config;
pub mod settings;
pub mod store;
pub mod theme;

use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::ext;
use crate::tools::ApprovalMode;
pub use providers_config::{ProviderCredential, ProviderEntry, ProvidersConfig};

/// Load and deserialize a YAML file, preserving I/O and parse errors.
/// Returns `Err` with a human-readable message that includes the file path.
pub(crate) fn load_yaml<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let raw = raw.trim_start_matches('\u{feff}');
    serde_yaml::from_str(raw).map_err(|e| format!("parse error in {}: {e}", path.display()))
}

/// Config / Lua / DB root.
///
/// Resolution order:
/// 1. `$BONE_DIR` — explicit absolute (or relative) override
/// 2. `$XDG_CONFIG_HOME/bone-rust`
/// 3. `$HOME/.bone-rust` (or `$USERPROFILE` on Windows)
///
/// Fails closed when none of these are set (no shared `/tmp/.bone-rust` fallback).
pub fn bone_dir() -> PathBuf {
    try_bone_dir().unwrap_or_else(|| {
        panic!(
            "bone: neither $BONE_DIR, $HOME, $USERPROFILE nor $XDG_CONFIG_HOME is set; \
             set BONE_DIR to a config root"
        )
    })
}

/// Like [`bone_dir`] but returns `None` when no config root can be resolved.
/// Use for best-effort bootstrap (e.g. deps marker) so `--help` still works
/// in a stripped environment.
pub fn try_bone_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("BONE_DIR") {
        let path = PathBuf::from(dir);
        if !path.as_os_str().is_empty() {
            return Some(path);
        }
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(xdg).join("bone-rust"));
    }
    if let Ok(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
        return Some(PathBuf::from(home).join(".bone-rust"));
    }
    None
}

pub fn providers_path() -> PathBuf {
    bone_dir().join("providers.yaml")
}

pub fn command_policy_path() -> PathBuf {
    bone_dir().join("command-policy.yaml")
}

/// Runtime configuration resolved from the daemon-owned [`store::ConfigStore`].
/// Canonical values are persisted across `config.yaml` and its peer documents.
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
    /// Input composer preset selected in `/config`; `None` keeps the init.lua preset.
    pub input_preset: Option<String>,
}

pub fn default_enabled_tools() -> Vec<String> {
    ["read_file", "create_file", "edit_file", "shell"]
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
            input_preset: None,
        }
    }
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
}

const DEFAULT_COMMAND_POLICY: &str = include_str!("../../default-command-policy.yaml");
const DEFAULT_AGENTS_MD: &str = include_str!("../../defaults/AGENTS.md");
const DEFAULT_CORE_DOCS: &[(&str, &str)] = &[
    (
        "architecture.md",
        include_str!("../../defaults/docs/architecture.md"),
    ),
    (
        "configuration.md",
        include_str!("../../defaults/docs/configuration.md"),
    ),
    (
        "extension-api.md",
        include_str!("../../defaults/docs/extension-api.md"),
    ),
    ("agents.md", include_str!("../../defaults/docs/agents.md")),
    ("ui.md", include_str!("../../defaults/docs/ui.md")),
    (
        "development.md",
        include_str!("../../defaults/docs/development.md"),
    ),
];

pub fn seed_command_policy_if_missing() -> Result<(), String> {
    let path = command_policy_path();
    match fs::metadata(&path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            domains::write_bytes(&path, DEFAULT_COMMAND_POLICY.as_bytes(), None)
                .map_err(|error| format!("cannot create {}: {error}", path.display()))?;
        }
        Err(error) => return Err(format!("cannot inspect {}: {error}", path.display())),
    }
    crate::tools::command_policy::validate_command_policy_path(&path)
        .map_err(|error| error.to_string())
}

/// Keep the application-owned agent reference synchronized with this build.
pub fn sync_agents_md() {
    let path = bone_dir().join("AGENTS.md");
    sync_bundled_file(&path, DEFAULT_AGENTS_MD);
}

/// Keep the bundled core reference documents synchronized with this build.
pub fn sync_core_docs() {
    let docs_dir = bone_dir().join("docs");
    for (name, content) in DEFAULT_CORE_DOCS {
        sync_bundled_file(&docs_dir.join(name), content);
    }
}

fn sync_bundled_file(path: &Path, content: &str) {
    if fs::read_to_string(path).is_ok_and(|current| current == content) {
        return;
    }
    seed_file_forced(path, content);
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
        crate::ext::ctx::runtime_warn(format!(
            "bone: warning: could not create {}: {e}",
            parent.display()
        ));
        return;
    }
    if let Err(e) = fs::write(path, content) {
        crate::ext::ctx::runtime_warn(format!(
            "bone: warning: could not write {}: {e}",
            path.display()
        ));
    }
}
/// The onboarding wizard's persisted choices: which bundled tools/commands the
/// user opted into. Doubles as the "already onboarded" marker — its presence
/// means setup has run. Absent it, seeding falls back to "seed everything".
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SetupSelection {
    /// Chosen tool filenames, e.g. `["subagent.lua", "web_search.lua"]`.
    pub tools: Vec<String>,
    /// Chosen command filenames, e.g. `["compact.lua"]`.
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
/// and Lua libraries). Idempotent.
pub fn seed_base() -> Result<(), String> {
    seed_command_policy_if_missing()?;
    sync_agents_md();
    sync_core_docs();
    migrate_memory_to_catalog(&bone_dir());
    ext::seed_default_lua_libs(&bone_dir().join("lua/lib"), None, false);
    Ok(())
}

const MEMORY_CATALOG_MIGRATION_MARKER: &str = ".memory-catalog-migrated";
const LEGACY_BUNDLED_MEMORY_COMMAND_SHA256: &str =
    "4da7cd58831fa28cedeec77ade6bdce907d95c7fa667b8c663dcf3ceeefa0ec8";

fn has_sha256(path: &Path, expected: &str) -> bool {
    let Ok(content) = fs::read(path) else {
        return false;
    };
    format!("{:x}", Sha256::digest(content)) == expected
}

/// One-time, data-preserving migration for the extraction of `/memory` from
/// bundled defaults. A known bundled command is renamed to a non-loadable backup;
/// catalog-installed and customized commands are left untouched. Legacy
/// `memory.md` is copied only when scoped global memory does not already exist.
fn migrate_memory_to_catalog(dir: &Path) {
    migrate_memory_to_catalog_with_hash(dir, LEGACY_BUNDLED_MEMORY_COMMAND_SHA256);
}

fn migrate_memory_to_catalog_with_hash(dir: &Path, bundled_command_sha256: &str) {
    let marker = dir.join(MEMORY_CATALOG_MIGRATION_MARKER);
    if marker.exists() {
        return;
    }

    let legacy = dir.join("memory.md");
    let scoped = dir.join("memory/global.md");
    let installed_command = dir.join("lua/commands/memory.lua");
    let bundled_command = has_sha256(&installed_command, bundled_command_sha256);
    let has_memory = legacy.exists() || dir.join("memory").exists();

    if legacy.exists() && !scoped.exists() {
        let Some(parent) = scoped.parent() else {
            return;
        };
        if let Err(e) =
            fs::create_dir_all(parent).and_then(|_| fs::copy(&legacy, &scoped).map(|_| ()))
        {
            crate::ext::ctx::runtime_warn(format!(
                "bone: warning: could not copy legacy memory.md to memory/global.md: {e}"
            ));
            return;
        }
    }

    if bundled_command {
        let backup = dir.join("lua/commands/memory.lua.bundled-backup");
        if let Err(e) = fs::rename(&installed_command, &backup) {
            crate::ext::ctx::runtime_warn(format!(
                "bone: warning: could not back up legacy lua/commands/memory.lua: {e}"
            ));
            return;
        }
    }

    if has_memory || bundled_command {
        let notice = if bundled_command {
            "bone: /memory is now an optional bone-catalog extension; the legacy memory.lua command was backed up and existing memory data was preserved"
        } else {
            "bone: /memory is now an optional bone-catalog extension; existing memory data was preserved"
        };
        crate::ext::ctx::runtime_warn(notice);
    }
    if let Err(e) = fs::write(&marker, "memory moved to bone-catalog\n") {
        crate::ext::ctx::runtime_warn(format!(
            "bone: warning: could not record /memory migration notice: {e}"
        ));
    }
}

/// Seed base config plus default tools, filtered by the onboarding selection.
/// `None` seeds every bundled tool (default / upgrade behavior).
pub fn seed_all_with(selection: Option<&SetupSelection>) -> Result<(), String> {
    seed_base()?;
    let allow = selection.map(SetupSelection::tool_set);
    ext::seed_default_lua_tools(&bone_dir().join("lua/tools"), allow.as_ref(), false);
    Ok(())
}

/// Seed using whatever selection is persisted on disk (or all, if none).
pub fn seed_all_with_persisted() -> Result<(), String> {
    seed_all_with(load_setup_selection().as_ref())
}

/// The user's `init.lua` choice in the onboarding wizard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitChoice {
    /// Banner wiring plus a starter sub-agent in `subagents.yaml`.
    Populated,
    /// Minimal placeholder.
    Blank,
    /// Leave the existing `init.lua` untouched (offered only when one exists).
    Keep,
}

fn seed_starter_subagent() -> std::io::Result<()> {
    let mut subagents = domains::load_subagents()
        .map_err(std::io::Error::other)?
        .unwrap_or_default()
        .subagents;
    subagents
        .entry("researcher".into())
        .or_insert_with(|| settings::SubagentSettings {
            description: "Investigates a question across the codebase and reports concise findings."
                .into(),
            system_prompt: Some(
                "You are a focused research agent. Investigate the assigned task thoroughly using the available tools, then report concrete findings with file:line references. Do not make edits."
                    .into(),
            ),
            ..Default::default()
        });
    domains::persist_subagents(&subagents).map_err(std::io::Error::other)
}

/// Persist the wizard's results and materialize them on disk: the selection
/// file (also the onboarding marker), the chosen `init.lua`, canonical starter
/// sub-agent configuration, and seeded tools/commands filtered to the selection.
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
    if init == InitChoice::Populated {
        seed_starter_subagent()?;
    }

    seed_base().map_err(std::io::Error::other)?;
    ext::seed_default_lua_tools(
        &bone_dir().join("lua/tools"),
        Some(&selection.tool_set()),
        false,
    );
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
    // Codex auth lives under the user home, not under bone_dir (which may be
    // `$XDG_CONFIG_HOME/bone-rust` — its parent is not `$HOME`).
    let path = dirs::home_dir()
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
        crate::ext::ctx::runtime_warn(format!(
            "bone: warning: provider '{}' not found in {}",
            provider_id,
            providers_path().display()
        ));
        return;
    };

    if !entry.api_key.is_empty()
        || is_local_base_url(&entry.base_url)
        || (entry.handler == "codex" && has_codex_auth_token())
        || (entry.handler == "grok_build" && crate::llm::providers::grok_build::has_cached_auth())
    {
        return;
    }
    if entry.handler == "grok_build" {
        crate::ext::ctx::runtime_warn(
            "bone: warning: Grok subscription is not authenticated; run `grok login`.",
        );
    } else {
        crate::ext::ctx::runtime_warn(format!(
            "bone: warning: provider '{}' has no API key configured. Edit {} and add your API key.",
            provider_id,
            providers_path().display()
        ));
    }
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;

//! User configuration loading: YAML config files, custom configs, and provider entries.

pub mod custom;
pub mod providers_config;
pub mod settings;

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

pub fn bone_dir() -> PathBuf {
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
    /// Input composer preset selected in `/config`; `None` keeps the init.lua preset.
    pub input_preset: Option<String>,
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

pub fn seed_command_policy_if_missing() {
    let path = command_policy_path();
    seed_file_if_missing(&path, DEFAULT_COMMAND_POLICY);
}

/// Keep the application-owned agent reference synchronized with this build.
/// User-authored instructions belong in `AGENTS.local.md` and are never touched.
///
/// If an existing `AGENTS.md` diverges from the bundled reference and
/// `AGENTS.local.md` is missing, the previous content is copied there once so
/// in-place customizations are not silently lost on upgrade.
pub fn sync_agents_md() {
    let dir = bone_dir();
    let path = dir.join("AGENTS.md");
    preserve_divergent_agents_md(&path, &dir.join("AGENTS.local.md"), DEFAULT_AGENTS_MD);
    sync_bundled_file(&path, DEFAULT_AGENTS_MD);
}

/// One-time migration: when `agents_path` exists with content different from
/// `bundled` and `local_path` is absent, copy the old content to `local_path`.
fn preserve_divergent_agents_md(agents_path: &Path, local_path: &Path, bundled: &str) {
    if local_path.exists() {
        return;
    }
    let Ok(existing) = fs::read_to_string(agents_path) else {
        return;
    };
    if existing == bundled {
        return;
    }
    match fs::write(local_path, &existing) {
        Ok(()) => {
            crate::ext::ctx::runtime_warn(
                "bone: previous AGENTS.md customizations preserved in AGENTS.local.md",
            );
        }
        Err(e) => {
            crate::ext::ctx::runtime_warn(format!(
                "bone: warning: could not preserve previous AGENTS.md to AGENTS.local.md: {e}"
            ));
        }
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
    sync_agents_md();
    custom::seed_builtin_pages(None, false);
    ext::seed_default_lua_libs(&bone_dir().join("lua/lib"), None, false);
}

/// Seed base config plus default tools, filtered by the onboarding selection.
/// `None` seeds every bundled tool (default / upgrade behavior).
pub fn seed_all_with(selection: Option<&SetupSelection>) {
    seed_base();
    let allow = selection.map(SetupSelection::tool_set);
    ext::seed_default_lua_tools(&bone_dir().join("lua/tools"), allow.as_ref(), false);
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
mod tests {
    use super::{preserve_divergent_agents_md, sync_bundled_file};
    use std::fs;

    #[test]
    fn bundled_file_is_created_and_stale_content_is_replaced() {
        let dir = std::env::temp_dir().join(format!(
            "bone-sync-bundled-file-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let path = dir.join("AGENTS.md");
        let local_path = dir.join("AGENTS.local.md");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&local_path, "user instructions").unwrap();

        sync_bundled_file(&path, "version 1");
        assert_eq!(fs::read_to_string(&path).unwrap(), "version 1");

        fs::write(&path, "stale").unwrap();
        sync_bundled_file(&path, "version 2");
        assert_eq!(fs::read_to_string(&path).unwrap(), "version 2");
        assert_eq!(
            fs::read_to_string(&local_path).unwrap(),
            "user instructions"
        );

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn divergent_agents_md_is_preserved_to_local_when_missing() {
        let dir = std::env::temp_dir().join(format!(
            "bone-sync-agents-migrate-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let path = dir.join("AGENTS.md");
        let local_path = dir.join("AGENTS.local.md");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, "my custom instructions").unwrap();
        assert!(!local_path.exists());

        let bundled = "bundled version";
        preserve_divergent_agents_md(&path, &local_path, bundled);
        sync_bundled_file(&path, bundled);

        assert_eq!(fs::read_to_string(&path).unwrap(), bundled);
        assert_eq!(
            fs::read_to_string(&local_path).unwrap(),
            "my custom instructions"
        );

        // Second pass must not overwrite an existing AGENTS.local.md.
        fs::write(&path, "stale again").unwrap();
        preserve_divergent_agents_md(&path, &local_path, bundled);
        sync_bundled_file(&path, bundled);
        assert_eq!(
            fs::read_to_string(&local_path).unwrap(),
            "my custom instructions"
        );

        fs::remove_dir_all(dir).unwrap();
    }
}

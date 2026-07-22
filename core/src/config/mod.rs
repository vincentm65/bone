//! User configuration loading: YAML config files, custom configs, and provider entries.

pub mod custom;
pub mod domains;
pub mod error;
mod migration;
pub mod providers_config;
pub mod settings;
pub mod store;

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
pub fn sync_agents_md() {
    let path = bone_dir().join("AGENTS.md");
    sync_bundled_file(&path, DEFAULT_AGENTS_MD);
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
pub fn seed_base() {
    seed_command_policy_if_missing();
    sync_agents_md();
    migrate_memory_to_catalog(&bone_dir());
    ext::seed_default_lua_libs(&bone_dir().join("lua/lib"), None, false);
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
mod tests {
    use super::{
        InitChoice, SetupSelection, apply_onboarding, domains, migrate_memory_to_catalog,
        migrate_memory_to_catalog_with_hash, settings::SubagentSettings, sync_bundled_file,
    };
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn migration_test_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "bone-memory-catalog-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ))
    }

    fn sha256(content: &[u8]) -> String {
        format!("{:x}", Sha256::digest(content))
    }

    fn with_test_bone_dir(test: impl FnOnce(&Path)) {
        let _guard = crate::util::test_env_lock();
        let dir = tempfile::tempdir().unwrap();
        let old_bone_dir = std::env::var_os("BONE_DIR");
        // SAFETY: held under test_env_lock; restored below.
        unsafe { std::env::set_var("BONE_DIR", dir.path()) };

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| test(dir.path())));

        match old_bone_dir {
            Some(value) => unsafe { std::env::set_var("BONE_DIR", value) },
            None => unsafe { std::env::remove_var("BONE_DIR") },
        }
        if let Err(payload) = result {
            std::panic::resume_unwind(payload);
        }
    }

    fn empty_selection() -> SetupSelection {
        SetupSelection {
            tools: Vec::new(),
            commands: Vec::new(),
        }
    }

    #[test]
    fn populated_onboarding_writes_banner_and_canonical_researcher() {
        with_test_bone_dir(|dir| {
            apply_onboarding(&empty_selection(), InitChoice::Populated).unwrap();

            assert_eq!(
                fs::read_to_string(dir.join("init.lua")).unwrap(),
                "-- Bone init.lua\nrequire(\"banner\")\n"
            );
            let config = domains::load_subagents().unwrap().unwrap();
            assert_eq!(config.version, 1);
            assert_eq!(config.subagents.len(), 1);
            assert_eq!(
                config.subagents.get("researcher"),
                Some(&SubagentSettings {
                    description:
                        "Investigates a question across the codebase and reports concise findings."
                            .into(),
                    system_prompt: Some(
                        "You are a focused research agent. Investigate the assigned task thoroughly using the available tools, then report concrete findings with file:line references. Do not make edits."
                            .into(),
                    ),
                    ..Default::default()
                })
            );
        });
    }

    #[test]
    fn populated_onboarding_preserves_existing_subagents() {
        with_test_bone_dir(|_| {
            let expected = BTreeMap::from([
                (
                    "reviewer".into(),
                    SubagentSettings {
                        description: "Reviews changes".into(),
                        system_prompt: Some("Review only".into()),
                        ..Default::default()
                    },
                ),
                (
                    "researcher".into(),
                    SubagentSettings {
                        description: "My custom researcher".into(),
                        system_prompt: Some("Use my instructions".into()),
                        provider: Some("custom-provider".into()),
                        model: Some("custom-model".into()),
                        approval: "danger".into(),
                        timeout_ms: Some(42_000),
                        max_concurrency: Some(3),
                        enabled: false,
                    },
                ),
            ]);
            domains::persist_subagents(&expected).unwrap();

            apply_onboarding(&empty_selection(), InitChoice::Populated).unwrap();

            assert_eq!(
                domains::load_subagents().unwrap().unwrap().subagents,
                expected
            );
        });
    }

    #[test]
    fn blank_and_keep_onboarding_do_not_modify_subagents() {
        with_test_bone_dir(|dir| {
            apply_onboarding(&empty_selection(), InitChoice::Blank).unwrap();
            assert!(!dir.join("subagents.yaml").exists());

            let subagents =
                "version: 1\nsubagents:\n  existing:\n    description: Existing agent\n";
            fs::write(dir.join("subagents.yaml"), subagents).unwrap();
            fs::write(dir.join("init.lua"), "-- existing init\n").unwrap();

            apply_onboarding(&empty_selection(), InitChoice::Keep).unwrap();

            assert_eq!(
                fs::read_to_string(dir.join("subagents.yaml")).unwrap(),
                subagents
            );
            assert_eq!(
                fs::read_to_string(dir.join("init.lua")).unwrap(),
                "-- existing init\n"
            );
        });
    }

    #[test]
    fn bone_dir_prefers_bone_dir_env() {
        let _guard = crate::util::test_env_lock();

        let dir = std::env::temp_dir().join(format!(
            "bone-dir-env-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let old_bone = std::env::var_os("BONE_DIR");
        let old_xdg = std::env::var_os("XDG_CONFIG_HOME");
        // SAFETY: held under test_env_lock; restored below.
        unsafe {
            std::env::set_var("BONE_DIR", &dir);
            std::env::set_var("XDG_CONFIG_HOME", "/should/not/win");
        }
        let got = super::bone_dir();
        match old_bone {
            Some(v) => unsafe { std::env::set_var("BONE_DIR", v) },
            None => unsafe { std::env::remove_var("BONE_DIR") },
        }
        match old_xdg {
            Some(v) => unsafe { std::env::set_var("XDG_CONFIG_HOME", v) },
            None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
        }
        assert_eq!(got, dir);
    }

    #[test]
    fn bone_dir_uses_xdg_when_bone_dir_unset() {
        let _guard = crate::util::test_env_lock();

        let xdg = std::env::temp_dir().join(format!(
            "bone-dir-xdg-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let old_bone = std::env::var_os("BONE_DIR");
        let old_xdg = std::env::var_os("XDG_CONFIG_HOME");
        unsafe {
            std::env::remove_var("BONE_DIR");
            std::env::set_var("XDG_CONFIG_HOME", &xdg);
        }
        let got = super::bone_dir();
        match old_bone {
            Some(v) => unsafe { std::env::set_var("BONE_DIR", v) },
            None => unsafe { std::env::remove_var("BONE_DIR") },
        }
        match old_xdg {
            Some(v) => unsafe { std::env::set_var("XDG_CONFIG_HOME", v) },
            None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
        }
        assert_eq!(got, xdg.join("bone-rust"));
    }

    #[test]
    fn bundled_file_is_created_and_stale_content_is_replaced() {
        let dir = std::env::temp_dir().join(format!(
            "bone-sync-bundled-file-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let path = dir.join("AGENTS.md");
        fs::create_dir_all(&dir).unwrap();

        sync_bundled_file(&path, "version 1");
        assert_eq!(fs::read_to_string(&path).unwrap(), "version 1");

        fs::write(&path, "stale").unwrap();
        sync_bundled_file(&path, "version 2");
        assert_eq!(fs::read_to_string(&path).unwrap(), "version 2");

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn clean_memory_migration_marks_complete_before_catalog_install() {
        let dir = migration_test_dir("clean");
        fs::create_dir_all(&dir).unwrap();

        migrate_memory_to_catalog(&dir);
        assert!(dir.join(".memory-catalog-migrated").exists());

        let command = dir.join("lua/commands/memory.lua");
        fs::create_dir_all(command.parent().unwrap()).unwrap();
        fs::write(&command, "-- catalog command").unwrap();
        migrate_memory_to_catalog(&dir);
        assert_eq!(fs::read_to_string(&command).unwrap(), "-- catalog command");

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn known_bundled_memory_command_is_backed_up() {
        let dir = migration_test_dir("bundled");
        let command = dir.join("lua/commands/memory.lua");
        fs::create_dir_all(command.parent().unwrap()).unwrap();
        let bundled = b"-- bundled command";
        fs::write(&command, bundled).unwrap();

        migrate_memory_to_catalog_with_hash(&dir, &sha256(bundled));

        assert!(!command.exists());
        assert_eq!(
            fs::read(dir.join("lua/commands/memory.lua.bundled-backup")).unwrap(),
            bundled
        );
        assert!(dir.join(".memory-catalog-migrated").exists());

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn catalog_memory_command_present_before_migration_is_preserved() {
        let dir = migration_test_dir("catalog");
        let command = dir.join("lua/commands/memory.lua");
        fs::create_dir_all(command.parent().unwrap()).unwrap();
        fs::write(&command, "-- catalog command").unwrap();

        migrate_memory_to_catalog(&dir);

        assert_eq!(fs::read_to_string(&command).unwrap(), "-- catalog command");
        assert!(dir.join(".memory-catalog-migrated").exists());

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn user_modified_bundled_memory_command_is_preserved() {
        let dir = migration_test_dir("modified");
        let command = dir.join("lua/commands/memory.lua");
        fs::create_dir_all(command.parent().unwrap()).unwrap();
        let bundled = b"-- bundled command";
        let modified = b"-- bundled command\n-- user customization";
        fs::write(&command, modified).unwrap();

        migrate_memory_to_catalog_with_hash(&dir, &sha256(bundled));

        assert_eq!(fs::read(&command).unwrap(), modified);
        assert!(dir.join(".memory-catalog-migrated").exists());

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn memory_catalog_migration_does_not_overwrite_scoped_data() {
        let dir = migration_test_dir("scoped");
        fs::create_dir_all(dir.join("memory")).unwrap();
        fs::write(dir.join("memory.md"), "legacy memory").unwrap();
        fs::write(dir.join("memory/global.md"), "scoped memory").unwrap();

        migrate_memory_to_catalog(&dir);

        assert_eq!(
            fs::read_to_string(dir.join("memory/global.md")).unwrap(),
            "scoped memory"
        );
        assert!(dir.join(".memory-catalog-migrated").exists());

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_memory_command_backup_leaves_migration_unmarked() {
        let dir = migration_test_dir("failed-backup");
        let command = dir.join("lua/commands/memory.lua");
        fs::create_dir_all(command.parent().unwrap()).unwrap();
        let bundled = b"-- bundled command";
        fs::write(&command, bundled).unwrap();
        fs::create_dir(dir.join("lua/commands/memory.lua.bundled-backup")).unwrap();

        migrate_memory_to_catalog_with_hash(&dir, &sha256(bundled));

        assert!(command.exists());
        assert!(!dir.join(".memory-catalog-migrated").exists());

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn memory_catalog_migration_copies_legacy_data() {
        let dir = migration_test_dir("legacy-data");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("memory.md"), "legacy memory").unwrap();

        migrate_memory_to_catalog(&dir);

        assert_eq!(
            fs::read_to_string(dir.join("memory/global.md")).unwrap(),
            "legacy memory"
        );
        assert!(dir.join(".memory-catalog-migrated").exists());

        fs::remove_dir_all(dir).unwrap();
    }
}

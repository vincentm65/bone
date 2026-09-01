//! Canonical settings in `~/.bone-rust/config.yaml`.
//!
//! Provides a versioned, validated, atomically persisted YAML file for the
//! canonical subset of configuration: general toggles, UI/input/spinner fields,
//! theme, and keymaps.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
#[cfg(test)]
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::bone_dir;

#[derive(Debug)]
pub enum SettingsError {
    Io(std::io::Error),
    Lock {
        operation: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    Parse(String),
    BadVersion(u8),
    Validation(String),
}

impl std::fmt::Display for SettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "settings I/O error: {e}"),
            Self::Lock {
                operation,
                path,
                source,
            } => write!(
                f,
                "cannot {operation} settings lock {}: {source}",
                path.display()
            ),
            Self::Parse(s) => write!(f, "settings parse error: {s}"),
            Self::BadVersion(v) => write!(f, "unsupported settings version {v}; expected 2"),
            Self::Validation(s) => write!(f, "settings validation error: {s}"),
        }
    }
}

impl std::error::Error for SettingsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) | Self::Lock { source: e, .. } => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for SettingsError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

// ── Path ─────────────────────────────────────────────────────────────────────

pub fn settings_path() -> PathBuf {
    bone_dir().join("config.yaml")
}

// ── Top-level schema ─────────────────────────────────────────────────────────

/// Scalar value persisted for an extension-owned setting. Null and structured
/// values are intentionally unsupported by the initial registry contract.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ExtensionValue {
    Bool(bool),
    Number(f64),
    String(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SubagentSettings {
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default = "default_approval")]
    pub approval: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for SubagentSettings {
    fn default() -> Self {
        Self {
            description: String::new(),
            system_prompt: None,
            provider: None,
            model: None,
            approval: default_approval(),
            timeout_ms: None,
            enabled: true,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnablementSettings {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoneSettings {
    pub version: u8,
    #[serde(default)]
    pub general: GeneralSettings,
    #[serde(default)]
    pub ui: UiSettings,
    #[serde(default)]
    pub theme: ThemeSettings,
    #[serde(default)]
    pub tools: EnablementSettings,
    #[serde(default)]
    pub commands: EnablementSettings,
    #[serde(default)]
    pub keymaps: KeymapSettings,
}

impl Default for BoneSettings {
    fn default() -> Self {
        Self {
            version: 2,
            general: GeneralSettings::default(),
            ui: UiSettings::default(),
            theme: ThemeSettings::default(),
            tools: EnablementSettings::default(),
            commands: EnablementSettings::default(),
            keymaps: KeymapSettings::default(),
        }
    }
}

fn shipped_settings() -> &'static BoneSettings {
    static SETTINGS: OnceLock<BoneSettings> = OnceLock::new();
    SETTINGS.get_or_init(|| {
        serde_yaml::from_str(include_str!("../../defaults/config.yaml"))
            .expect("bundled default config.yaml must be valid")
    })
}

pub fn shipped_system_prompt() -> &'static str {
    shipped_settings()
        .general
        .system_prompt
        .as_deref()
        .expect("bundled default config.yaml must define general.system_prompt")
}

// ── General ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeneralSettings {
    #[serde(default = "default_approval")]
    pub approval: String,
    #[serde(default)]
    pub show_reasoning: bool,
    #[serde(default)]
    pub system_prompt: Option<String>,
}

fn default_approval() -> String {
    "safe".to_string()
}

impl GeneralSettings {
    pub fn system_prompt(&self) -> &str {
        self.system_prompt
            .as_deref()
            .expect("general.system_prompt must be hydrated when settings load")
    }
}

impl Default for GeneralSettings {
    fn default() -> Self {
        Self {
            approval: default_approval(),
            show_reasoning: false,
            system_prompt: Some(shipped_system_prompt().to_owned()),
        }
    }
}

// ── UI / input / status / spinner ────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiInputBorderSettings {
    pub horizontal: Option<String>,
    pub vertical: Option<String>,
    pub top_left: Option<String>,
    pub top_right: Option<String>,
    pub bottom_left: Option<String>,
    pub bottom_right: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiInputSettings {
    #[serde(default)]
    pub preset: Option<String>,
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default = "default_true")]
    pub show_prefix: bool,
    #[serde(default)]
    pub horizontal_padding: Option<u16>,
    #[serde(default)]
    pub vertical_padding: Option<u16>,
    #[serde(default)]
    pub fill: Option<bool>,
    #[serde(default)]
    pub border: UiInputBorderSettings,
}

impl Default for UiInputSettings {
    fn default() -> Self {
        Self {
            preset: None,
            prefix: None,
            show_prefix: true,
            horizontal_padding: None,
            vertical_padding: None,
            fill: None,
            border: UiInputBorderSettings::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiSettings {
    #[serde(default)]
    pub input: UiInputSettings,

    // Status-bar visibility toggles
    #[serde(default = "default_true")]
    pub status_show_model: bool,
    #[serde(default = "default_true")]
    pub status_show_approval: bool,
    #[serde(default = "default_true")]
    pub status_show_tokens_curr: bool,
    #[serde(default = "default_true")]
    pub status_show_tokens_in: bool,
    #[serde(default = "default_true")]
    pub status_show_tokens_out: bool,
    #[serde(default = "default_true")]
    pub status_show_tokens_total: bool,
    #[serde(default = "default_true")]
    pub status_show_queue: bool,
    #[serde(default = "default_true")]
    pub status_show_spinner: bool,
    #[serde(default = "default_true")]
    pub status_show_timer: bool,

    // Spinner configuration
    #[serde(default = "default_spinner_style")]
    pub spinner_style: String,
    #[serde(default = "default_spinner_text")]
    pub spinner_text: String,
    #[serde(default)]
    pub spinner_custom: String,
    #[serde(default)]
    pub spinner_speed: u64,
    #[serde(default = "default_true")]
    pub spinner_text_rotate: bool,
    #[serde(default)]
    pub spinner_text_speed: u64,
}

fn default_true() -> bool {
    true
}
fn default_spinner_style() -> String {
    "braille".to_string()
}
fn default_spinner_text() -> String {
    "thinking".to_string()
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            input: UiInputSettings::default(),
            status_show_model: true,
            status_show_approval: true,
            status_show_tokens_curr: true,
            status_show_tokens_in: true,
            status_show_tokens_out: true,
            status_show_tokens_total: true,
            status_show_queue: true,
            status_show_spinner: true,
            status_show_timer: true,
            spinner_style: default_spinner_style(),
            spinner_text: default_spinner_text(),
            spinner_custom: String::new(),
            spinner_speed: 0,
            spinner_text_rotate: true,
            spinner_text_speed: 0,
        }
    }
}

// ── Theme (unchanged shape) ──────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemePaletteSettings {
    pub bg: Option<String>,
    pub fg: Option<String>,
    pub muted: Option<String>,
    pub subtle: Option<String>,
    pub border: Option<String>,
    pub accent: Option<String>,
    pub good: Option<String>,
    pub warn: Option<String>,
    pub error: Option<String>,
    pub selection: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeShellSettings {
    pub program: Option<String>,
    pub separator: Option<String>,
    pub redirect: Option<String>,
    pub flag: Option<String>,
    pub string: Option<String>,
    pub variable: Option<String>,
    pub comment: Option<String>,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeSyntaxSettings {
    pub text: Option<String>,
    pub comment: Option<String>,
    pub string: Option<String>,
    pub number: Option<String>,
    pub constant: Option<String>,
    pub escape: Option<String>,
    pub regex: Option<String>,
    pub keyword: Option<String>,
    pub keyword_control: Option<String>,
    #[serde(rename = "type")]
    pub r#type: Option<String>,
    pub function_name: Option<String>,
    pub variable: Option<String>,
    pub tag: Option<String>,
    pub attribute: Option<String>,
    pub punctuation: Option<String>,
    pub subtle: Option<String>,
    pub markup: Option<String>,
    pub invalid: Option<String>,
}

/// A highlight is either a scalar color/reference or an explicit channel object.
/// Objects accept only `fg` for foreground roles, only `bg` for background roles,
/// and both channels for the composite `user_msg` role. Typography modifiers are
/// intentionally not part of the persisted theme schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum ThemeStyleSpec {
    Color(String),
    Style {
        fg: Option<String>,
        bg: Option<String>,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeSettings {
    /// Selected `lua/themes/<name>.lua` theme. The resolved fields below are
    /// persisted alongside it so frontends never need filesystem or Lua access.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub palette: ThemePaletteSettings,
    #[serde(default)]
    pub shell: ThemeShellSettings,
    #[serde(default)]
    pub syntax: ThemeSyntaxSettings,
    #[serde(default)]
    pub highlights: std::collections::BTreeMap<String, ThemeStyleSpec>,
    pub user_msg: Option<String>,
    pub user_msg_bg: Option<String>,
    pub status_text: Option<String>,
    pub input_border: Option<String>,
    pub system_msg: Option<String>,
    pub approval_safe: Option<String>,
    pub approval_danger: Option<String>,
    pub tool_call: Option<String>,
    pub tool_error: Option<String>,
    pub shell_program: Option<String>,
    pub shell_separator: Option<String>,
    pub shell_redirect: Option<String>,
    pub shell_flag: Option<String>,
    pub shell_string: Option<String>,
    pub shell_variable: Option<String>,
    pub shell_comment: Option<String>,
    pub shell_path: Option<String>,
    pub diff_removed: Option<String>,
    pub diff_added: Option<String>,
    pub thinking: Option<String>,
    pub markdown_marker: Option<String>,
    pub markdown_heading: Option<String>,
    pub markdown_link: Option<String>,
    pub markdown_inline_code: Option<String>,
    pub markdown_rule: Option<String>,
    pub markdown_table_border: Option<String>,
    pub markdown_table_header: Option<String>,
    pub chart: Option<String>,
    pub chart_empty: Option<String>,
    pub heat_low: Option<String>,
    pub heat_high: Option<String>,
    pub syntax_text: Option<String>,
    pub syntax_comment: Option<String>,
    pub syntax_string: Option<String>,
    pub syntax_number: Option<String>,
    pub syntax_constant: Option<String>,
    pub syntax_escape: Option<String>,
    pub syntax_regex: Option<String>,
    pub syntax_keyword: Option<String>,
    pub syntax_keyword_control: Option<String>,
    pub syntax_type: Option<String>,
    pub syntax_function: Option<String>,
    pub syntax_variable: Option<String>,
    pub syntax_tag: Option<String>,
    pub syntax_attribute: Option<String>,
    pub syntax_punctuation: Option<String>,
    pub syntax_subtle: Option<String>,
    pub syntax_markup: Option<String>,
    pub syntax_invalid: Option<String>,
}

// ── Keymaps ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyBinding {
    pub key: String,
    pub action: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeymapSettings {
    #[serde(default)]
    pub bindings: Vec<KeyBinding>,
}

// ── Cross-process advisory lock ──────────────────────────────────────────────

fn lock_path_for(settings_path: &Path) -> PathBuf {
    let mut path = settings_path.as_os_str().to_owned();
    path.push(".lock");
    path.into()
}

fn acquire_settings_write_lock(
    path: &Path,
) -> Result<(MutexGuard<'static, ()>, File), SettingsError> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let mutex = LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let lock_path = lock_path_for(path);
    let lock_error = |operation, source| SettingsError::Lock {
        operation,
        path: lock_path.clone(),
        source,
    };
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| lock_error("create parent directory for", error))?;
    }
    let file = File::options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|error| lock_error("open", error))?;
    fs2::FileExt::lock_exclusive(&file).map_err(|error| lock_error("acquire", error))?;
    Ok((mutex, file))
}

fn sparse_settings_value(settings: &BoneSettings) -> Result<serde_yaml::Value, SettingsError> {
    let mut value =
        serde_yaml::to_value(settings).map_err(|error| SettingsError::Parse(error.to_string()))?;
    let defaults = serde_yaml::to_value(BoneSettings::default())
        .map_err(|error| SettingsError::Parse(error.to_string()))?;
    prune_defaults(&mut value, &defaults, true, false);
    Ok(value)
}

fn prune_defaults(
    value: &mut serde_yaml::Value,
    defaults: &serde_yaml::Value,
    root: bool,
    general: bool,
) {
    let (serde_yaml::Value::Mapping(values), serde_yaml::Value::Mapping(default_values)) =
        (value, defaults)
    else {
        return;
    };

    let keys: Vec<_> = values.keys().cloned().collect();
    for key in keys {
        if (root && key.as_str() == Some("version"))
            || (general && key.as_str() == Some("system_prompt"))
        {
            continue;
        }
        let Some(default) = default_values.get(&key) else {
            continue;
        };
        let Some(current) = values.get_mut(&key) else {
            continue;
        };
        prune_defaults(
            current,
            default,
            false,
            root && key.as_str() == Some("general"),
        );
        let empty_mapping = matches!(current, serde_yaml::Value::Mapping(map) if map.is_empty());
        let preserved_container = root && key.as_str() == Some("general");
        if (!preserved_container && current == default) || empty_mapping {
            values.remove(&key);
        }
    }
}

// ── Settings wrapper ─────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Settings {
    pub(crate) inner: BoneSettings,
    pub(crate) revision: u64,
    subagents: BTreeMap<String, SubagentSettings>,
    extensions: BTreeMap<String, BTreeMap<String, ExtensionValue>>,
}

impl Settings {
    pub fn defaults() -> Self {
        Self {
            inner: BoneSettings::default(),
            revision: 0,
            subagents: BTreeMap::new(),
            extensions: BTreeMap::new(),
        }
    }

    pub fn resolved(&self) -> &BoneSettings {
        &self.inner
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn into_resolved(self) -> BoneSettings {
        self.inner
    }

    pub(crate) fn replace_domains(
        &mut self,
        subagents: BTreeMap<String, SubagentSettings>,
        extensions: BTreeMap<String, BTreeMap<String, ExtensionValue>>,
    ) {
        self.subagents = subagents;
        self.extensions = extensions;
    }

    pub(crate) fn subagents(&self) -> &BTreeMap<String, SubagentSettings> {
        &self.subagents
    }

    pub(crate) fn extensions(&self) -> &BTreeMap<String, BTreeMap<String, ExtensionValue>> {
        &self.extensions
    }

    /// Load `config.yaml` from the resolved Bone configuration directory. Returns
    /// `Ok(None)` when the file does not exist (caller should migrate), `Err(...)`
    /// when it exists but is corrupt or has a bad version.
    pub fn load() -> Result<Option<Self>, SettingsError> {
        Self::load_path(&settings_path())
    }

    fn load_path(path: &std::path::Path) -> Result<Option<Self>, SettingsError> {
        let (settings, hydrated) = Self::load_path_unlocked(path)?;
        if !hydrated {
            return Ok(settings);
        }

        let _guard = acquire_settings_write_lock(path)?;
        let (settings, hydrated) = Self::load_path_unlocked(path)?;
        if hydrated && let Some(settings) = settings.as_ref() {
            settings.write_path(path)?;
        }
        Ok(settings)
    }

    fn load_path_unlocked(path: &std::path::Path) -> Result<(Option<Self>, bool), SettingsError> {
        if !path.exists() {
            return Ok((None, false));
        }

        let raw = fs::read_to_string(path)?;
        let raw = raw.trim_start_matches('\u{feff}');
        let value: serde_yaml::Value =
            serde_yaml::from_str(raw).map_err(|e| SettingsError::Parse(e.to_string()))?;
        let hydrated = value
            .get("general")
            .and_then(|general| general.get("system_prompt"))
            .is_none_or(serde_yaml::Value::is_null);
        let mut inner: BoneSettings =
            serde_yaml::from_value(value).map_err(|e| SettingsError::Parse(e.to_string()))?;

        if inner.version != 2 {
            return Err(SettingsError::BadVersion(inner.version));
        }

        if hydrated {
            inner.general.system_prompt = Some(shipped_system_prompt().to_owned());
        }

        validate_general(&inner.general)?;
        validate_theme(&inner.theme)?;
        validate_keymaps(&inner.keymaps)?;

        Ok((
            Some(Self {
                inner,
                revision: 0,
                subagents: BTreeMap::new(),
                extensions: BTreeMap::new(),
            }),
            hydrated,
        ))
    }

    /// Atomically write to `config.yaml` via a same-directory temporary file.
    pub fn save(&self) -> Result<(), SettingsError> {
        self.save_path(&settings_path())
    }

    pub(crate) fn save_path(&self, path: &std::path::Path) -> Result<(), SettingsError> {
        let _guard = acquire_settings_write_lock(path)?;
        self.write_path(path)
    }

    pub(crate) fn sparse_yaml(&self) -> Result<String, SettingsError> {
        let sparse = sparse_settings_value(&self.inner)?;
        serde_yaml::to_string(&sparse).map_err(|e| SettingsError::Parse(e.to_string()))
    }

    fn write_path(&self, path: &std::path::Path) -> Result<(), SettingsError> {
        let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
        fs::create_dir_all(parent)?;

        let yaml = self.sparse_yaml()?;
        #[cfg(unix)]
        let permissions = {
            use std::os::unix::fs::PermissionsExt;
            Some(std::fs::Permissions::from_mode(0o600))
        };
        #[cfg(not(unix))]
        let permissions = None;
        crate::tools::write_atomic::write_atomic_sync(path, yaml.as_bytes(), permissions)
            .map_err(|error| SettingsError::Io(std::io::Error::other(error)))
    }

    /// Reload the latest file and commit one validated mutation while all
    /// in-process settings writers are serialized.
    fn update_path<F>(&mut self, path: &std::path::Path, mutate: F) -> Result<(), SettingsError>
    where
        F: FnOnce(&mut Self) -> Result<(), SettingsError>,
    {
        let _guard = acquire_settings_write_lock(path)?;
        let mut candidate = Self::load_path_unlocked(path)?
            .0
            .unwrap_or_else(|| self.clone());
        candidate.subagents = self.subagents.clone();
        candidate.extensions = self.extensions.clone();
        mutate(&mut candidate)?;
        validate_settings(&candidate.inner)?;
        candidate.revision = self.revision.saturating_add(1);
        candidate.write_path(path)?;
        *self = candidate;
        Ok(())
    }

    /// Persist one extension value against the latest settings document.
    pub fn set_extension_value_at(
        &mut self,
        path: &str,
        value: ExtensionValue,
        settings_path: &Path,
    ) -> Result<(), SettingsError> {
        let (namespace, key) = path.split_once('.').ok_or_else(|| {
            SettingsError::Validation("extension setting path must be namespace.key".into())
        })?;
        if namespace.is_empty() || key.is_empty() || key.contains('.') {
            return Err(SettingsError::Validation(
                "extension setting path must be namespace.key".into(),
            ));
        }
        self.update_path(settings_path, |candidate| {
            candidate
                .extensions
                .entry(namespace.to_string())
                .or_default()
                .insert(key.to_string(), value);
            Ok(())
        })
    }

    pub fn extension_value(&self, path: &str) -> Option<&ExtensionValue> {
        let (namespace, key) = path.split_once('.')?;
        self.extensions.get(namespace)?.get(key)
    }

    /// Read a canonical setting by dotted path (for example `general.approval`).
    pub fn get_path(&self, path: &str) -> Result<serde_json::Value, SettingsError> {
        let value =
            serde_json::to_value(&self.inner).map_err(|e| SettingsError::Parse(e.to_string()))?;
        json_path(&value, path)
            .cloned()
            .ok_or_else(|| SettingsError::Validation(format!("unknown setting: {path}")))
    }

    /// Validate, persist, and commit one canonical setting by dotted path.
    pub(crate) fn set_path_at(
        &mut self,
        path: &str,
        value: serde_json::Value,
        file: &std::path::Path,
    ) -> Result<(), SettingsError> {
        self.update_path(file, |candidate| {
            let mut document = serde_json::to_value(&candidate.inner)
                .map_err(|e| SettingsError::Parse(e.to_string()))?;
            let slot = json_path_mut(&mut document, path)
                .ok_or_else(|| SettingsError::Validation(format!("unknown setting: {path}")))?;
            *slot = value;
            candidate.inner = serde_json::from_value(document)
                .map_err(|e| SettingsError::Validation(format!("{path}: {e}")))?;
            if candidate.inner.general.system_prompt.is_none() {
                candidate.inner.general.system_prompt = Some(shipped_system_prompt().to_owned());
            }
            Ok(())
        })
    }

    /// Replace the resolved theme against the latest persisted document.
    pub(crate) fn replace_theme_at(
        &mut self,
        theme: ThemeSettings,
        file: &std::path::Path,
    ) -> Result<(), SettingsError> {
        self.update_path(file, |candidate| {
            candidate.inner.theme = theme;
            Ok(())
        })
    }

    /// Reset one canonical setting to its schema default and persist it.
    pub(crate) fn reset_path_at(
        &mut self,
        path: &str,
        file: &std::path::Path,
    ) -> Result<serde_json::Value, SettingsError> {
        let default = Settings::defaults().get_path(path)?;
        self.set_path_at(path, default.clone(), file)?;
        Ok(default)
    }

}

// ── Validation ───────────────────────────────────────────────────────────────

fn validate_settings(settings: &BoneSettings) -> Result<(), SettingsError> {
    if settings.version != 2 {
        return Err(SettingsError::BadVersion(settings.version));
    }
    validate_general(&settings.general)?;
    validate_theme(&settings.theme)?;
    validate_keymaps(&settings.keymaps)
}

fn validate_subagent_name(name: &str) -> Result<(), SettingsError> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(SettingsError::Validation(format!(
            "sub-agent name must contain only ASCII letters, digits, '-' or '_', got {name:?}"
        )));
    }
    Ok(())
}

fn validate_subagent(name: &str, agent: &SubagentSettings) -> Result<(), SettingsError> {
    validate_subagent_name(name)?;
    if agent.description.trim().is_empty() {
        return Err(SettingsError::Validation(format!(
            "subagents.{name}.description must not be empty"
        )));
    }
    if !matches!(agent.approval.as_str(), "safe" | "danger") {
        return Err(SettingsError::Validation(format!(
            "subagents.{name}.approval must be 'safe' or 'danger'"
        )));
    }
    if agent
        .timeout_ms
        .is_some_and(|timeout| timeout == 0 || timeout > 900_000)
    {
        return Err(SettingsError::Validation(format!(
            "subagents.{name}.timeout_ms must be between 1 and 900000"
        )));
    }
    Ok(())
}

pub(crate) fn validate_subagents(
    agents: &BTreeMap<String, SubagentSettings>,
) -> Result<(), SettingsError> {
    for (name, agent) in agents {
        validate_subagent(name, agent)?;
    }
    Ok(())
}

pub(crate) fn validate_theme(theme: &ThemeSettings) -> Result<(), SettingsError> {
    if let Some(name) = &theme.name
        && (name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
    {
        return Err(SettingsError::Validation(format!(
            "theme.name must contain only ASCII letters, digits, '-' or '_', got {name:?}"
        )));
    }
    let value =
        serde_json::to_value(theme).map_err(|e| SettingsError::Validation(e.to_string()))?;
    validate_theme_values(&value, "theme")?;
    for (name, spec) in &theme.highlights {
        let role = crate::config::theme::role(name).ok_or_else(|| {
            SettingsError::Validation(format!("theme.highlights.{name}: unknown role"))
        })?;
        if crate::config::theme::palette_name(name) {
            return Err(SettingsError::Validation(format!(
                "theme.highlights.{name}: palette roles belong under theme.palette"
            )));
        }
        match spec {
            ThemeStyleSpec::Color(value) => {
                validate_theme_color(value, &format!("theme.highlights.{name}"))?
            }
            ThemeStyleSpec::Style { fg, bg } => {
                if fg.is_none() && bg.is_none() {
                    return Err(SettingsError::Validation(format!(
                        "theme.highlights.{name}: style must specify fg or bg"
                    )));
                }
                if fg.is_some() && role.kind == crate::config::theme::RoleKind::Background {
                    return Err(SettingsError::Validation(format!(
                        "theme.highlights.{name}.fg is not valid for a background role"
                    )));
                }
                if bg.is_some() && role.kind == crate::config::theme::RoleKind::Foreground {
                    return Err(SettingsError::Validation(format!(
                        "theme.highlights.{name}.bg is not valid for a foreground role"
                    )));
                }
                if let Some(value) = fg {
                    validate_theme_color(value, &format!("theme.highlights.{name}.fg"))?;
                }
                if let Some(value) = bg {
                    validate_theme_color(value, &format!("theme.highlights.{name}.bg"))?;
                }
            }
        }
    }
    Ok(())
}

fn validate_theme_values(value: &serde_json::Value, path: &str) -> Result<(), SettingsError> {
    match value {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                if key != "name" && key != "highlights" {
                    validate_theme_values(value, &format!("{path}.{key}"))?;
                }
            }
        }
        serde_json::Value::String(value) => validate_theme_color(value, path)?,
        _ => {}
    }
    Ok(())
}

fn validate_theme_color(value: &str, path: &str) -> Result<(), SettingsError> {
    if crate::config::theme::parse_color(value).is_err()
        && !crate::config::theme::palette_name(value)
    {
        return Err(SettingsError::Validation(format!(
            "{path}: invalid color {value:?}"
        )));
    }
    Ok(())
}

fn json_path<'a>(value: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut current = value;
    for segment in path.split('.') {
        if segment.is_empty() {
            return None;
        }
        current = current.as_object()?.get(segment)?;
    }
    Some(current)
}

fn json_path_mut<'a>(
    value: &'a mut serde_json::Value,
    path: &str,
) -> Option<&'a mut serde_json::Value> {
    let mut current = value;
    for segment in path.split('.') {
        if segment.is_empty() {
            return None;
        }
        current = current.as_object_mut()?.get_mut(segment)?;
    }
    Some(current)
}

fn validate_approval(s: &str) -> Result<&str, SettingsError> {
    match s {
        "safe" | "danger" => Ok(s),
        other => Err(SettingsError::Validation(format!(
            "approval must be 'safe' or 'danger', got {other:?}"
        ))),
    }
}

fn validate_general(g: &GeneralSettings) -> Result<(), SettingsError> {
    validate_approval(&g.approval).map(|_| ())
}

fn validate_keymaps(k: &KeymapSettings) -> Result<(), SettingsError> {
    let mut keys = std::collections::HashSet::new();
    for (i, binding) in k.bindings.iter().enumerate() {
        if binding.key.is_empty() {
            return Err(SettingsError::Validation(format!(
                "keymaps.bindings[{i}].key must not be empty"
            )));
        }
        if binding.action.is_empty() {
            return Err(SettingsError::Validation(format!(
                "keymaps.bindings[{i}].action must not be empty"
            )));
        }
        if !keys.insert(&binding.key) {
            return Err(SettingsError::Validation(format!(
                "duplicate keymap binding: {:?}",
                binding.key
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;

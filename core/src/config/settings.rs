//! Canonical settings in `~/.bone-rust/config.yaml`.
//!
//! Provides a versioned, validated, atomically persisted YAML file for the
//! canonical subset of configuration: general toggles, UI/input/spinner fields,
//! theme, and keymaps.  `CustomConfigs` routes legacy `/config` page keys
//! through this module.

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
            Self::BadVersion(v) => write!(f, "unsupported settings version {v}; expected 1 or 2"),
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrency: Option<usize>,
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
            max_concurrency: None,
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
    /// Legacy migration inputs. Canonical values live in `subagents.yaml` and
    /// are never serialized back into `config.yaml`.
    #[serde(default, skip_serializing)]
    pub subagents: BTreeMap<String, SubagentSettings>,
    /// Legacy migration inputs. Canonical values live in `extensions.yaml` and
    /// are never serialized back into `config.yaml`.
    #[serde(default, skip_serializing)]
    pub extensions: BTreeMap<String, BTreeMap<String, ExtensionValue>>,
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
            subagents: BTreeMap::new(),
            extensions: BTreeMap::new(),
        }
    }
}

// ── General ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeneralSettings {
    #[serde(default = "default_approval")]
    pub approval: String,
    #[serde(default)]
    pub show_reasoning: bool,
}

fn default_approval() -> String {
    "safe".to_string()
}

impl Default for GeneralSettings {
    fn default() -> Self {
        Self {
            approval: default_approval(),
            show_reasoning: false,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum ThemeStyleSpec {
    Color(String),
    Style {
        fg: Option<String>,
        bg: Option<String>,
        bold: Option<bool>,
        italic: Option<bool>,
        underline: Option<bool>,
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
    pub tab_active: Option<String>,
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
    prune_defaults(&mut value, &defaults, true);
    Ok(value)
}

fn prune_defaults(value: &mut serde_yaml::Value, defaults: &serde_yaml::Value, root: bool) {
    let (serde_yaml::Value::Mapping(values), serde_yaml::Value::Mapping(default_values)) =
        (value, defaults)
    else {
        return;
    };

    let keys: Vec<_> = values.keys().cloned().collect();
    for key in keys {
        if root && key.as_str() == Some("version") {
            continue;
        }
        let Some(default) = default_values.get(&key) else {
            continue;
        };
        let Some(current) = values.get_mut(&key) else {
            continue;
        };
        prune_defaults(current, default, false);
        let empty_mapping = matches!(current, serde_yaml::Value::Mapping(map) if map.is_empty());
        if current == default || empty_mapping {
            values.remove(&key);
        }
    }
}

// ── Settings wrapper ─────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Settings {
    pub(crate) inner: BoneSettings,
    pub(crate) revision: u64,
}

impl Settings {
    pub fn defaults() -> Self {
        Self {
            inner: BoneSettings::default(),
            revision: 0,
        }
    }

    pub fn resolved(&self) -> &BoneSettings {
        &self.inner
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn validate(&self) -> Result<(), SettingsError> {
        validate_settings(&self.inner)
    }

    pub fn into_resolved(self) -> BoneSettings {
        self.inner
    }

    pub(crate) fn replace_domains(
        &mut self,
        subagents: BTreeMap<String, SubagentSettings>,
        extensions: BTreeMap<String, BTreeMap<String, ExtensionValue>>,
    ) {
        self.inner.subagents = subagents;
        self.inner.extensions = extensions;
    }

    /// Load `config.yaml` from the resolved Bone configuration directory. Returns
    /// `Ok(None)` when the file does not exist (caller should migrate), `Err(...)`
    /// when it exists but is corrupt or has a bad version.
    pub fn load() -> Result<Option<Self>, SettingsError> {
        Self::load_path(&settings_path())
    }

    fn load_path(path: &std::path::Path) -> Result<Option<Self>, SettingsError> {
        if !path.exists() {
            return Ok(None);
        }

        let raw = fs::read_to_string(path)?;
        let raw = raw.trim_start_matches('\u{feff}');

        let inner: BoneSettings =
            serde_yaml::from_str(raw).map_err(|e| SettingsError::Parse(e.to_string()))?;

        if !matches!(inner.version, 1 | 2) {
            return Err(SettingsError::BadVersion(inner.version));
        }

        validate_general(&inner.general)?;
        validate_theme(&inner.theme)?;
        validate_keymaps(&inner.keymaps)?;
        validate_subagents(&inner.subagents)?;

        Ok(Some(Self { inner, revision: 0 }))
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
        let mut candidate = Self::load_path(path)?.unwrap_or_else(|| self.clone());
        mutate(&mut candidate)?;
        validate_settings(&candidate.inner)?;
        candidate.revision = self.revision.saturating_add(1);
        candidate.write_path(path)?;
        self.inner = candidate.inner;
        self.revision = candidate.revision;
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
                .inner
                .extensions
                .entry(namespace.to_string())
                .or_default()
                .insert(key.to_string(), value);
            Ok(())
        })
    }

    pub fn extension_value(&self, path: &str) -> Option<&ExtensionValue> {
        let (namespace, key) = path.split_once('.')?;
        self.inner.extensions.get(namespace)?.get(key)
    }

    // ── Canonical key routing (legacy page keys → new hierarchy) ──────────

    /// Legacy page field keys routed through canonical settings.
    pub fn canonical_keys() -> Vec<(&'static str, &'static str)> {
        let mut keys = Vec::new();
        for k in &["approval_mode", "show_thinking", "input_preset"] {
            keys.push(("general", *k));
        }
        keys.push(("theme", "settings_json"));
        keys.push(("keymaps", "bindings_json"));
        for k in &[
            "status_show_model",
            "status_show_approval",
            "status_show_tokens_curr",
            "status_show_tokens_in",
            "status_show_tokens_out",
            "status_show_tokens_total",
            "status_show_queue",
            "status_show_spinner",
            "status_show_timer",
            "status_spinner_style",
            "status_spinner_text",
            "status_spinner_speed",
            "status_spinner_text_rotate",
            "status_spinner_text_speed",
            "status_spinner_text_custom",
        ] {
            keys.push(("status", *k));
        }
        keys
    }

    /// True if `(namespace, key)` is a canonical field.
    pub fn is_canonical(namespace: &str, key: &str) -> bool {
        Self::canonical_keys()
            .iter()
            .any(|(ns, k)| *ns == namespace && *k == key)
    }

    /// Get canonical value as display string.
    pub fn get_value(&self, namespace: &str, key: &str) -> String {
        match (namespace, key) {
            ("general", "approval_mode") => self.inner.general.approval.clone(),
            ("general", "show_thinking") => self.inner.general.show_reasoning.to_string(),
            ("general", "input_preset") => self.inner.ui.input.preset.clone().unwrap_or_default(),
            ("theme", "settings_json") => {
                serde_json::to_string(&self.inner.theme).unwrap_or_default()
            }
            ("keymaps", "bindings_json") => {
                serde_json::to_string(&self.inner.keymaps.bindings).unwrap_or_default()
            }
            ("status", k) => self.get_ui(k),
            _ => String::new(),
        }
    }

    /// Set one canonical value against the latest persisted document.
    pub fn set_value(
        &mut self,
        namespace: &str,
        key: &str,
        value: String,
    ) -> Result<(), SettingsError> {
        self.update_path(&settings_path(), |candidate| {
            match (namespace, key) {
                ("general", "approval_mode") => {
                    validate_approval(&value)?;
                    candidate.inner.general.approval = value;
                }
                ("general", "show_thinking") => {
                    candidate.inner.general.show_reasoning = parse_bool(&value)?;
                }
                ("general", "input_preset") => match value.as_str() {
                    "" => candidate.inner.ui.input.preset = None,
                    s @ ("custom" | "lines" | "box" | "filled") => {
                        candidate.inner.ui.input.preset = Some(s.to_string())
                    }
                    other => {
                        return Err(SettingsError::Validation(format!(
                            "input_preset must be custom/lines/box/filled, got {other:?}"
                        )));
                    }
                },
                ("theme", "settings_json") => {
                    candidate.inner.theme = serde_json::from_str(&value).map_err(|e| {
                        SettingsError::Validation(format!("theme JSON is invalid: {e}"))
                    })?;
                }
                ("keymaps", "bindings_json") => {
                    candidate.inner.keymaps.bindings =
                        serde_json::from_str(&value).map_err(|e| {
                            SettingsError::Validation(format!("keymap JSON is invalid: {e}"))
                        })?;
                }
                ("status", k) => candidate.set_ui(k, value)?,
                _ => {
                    return Err(SettingsError::Validation(format!(
                        "unknown canonical namespace: {namespace}"
                    )));
                }
            }
            Ok(())
        })
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

    /// Cycle enum/bool fields to next value.
    pub fn cycle_field(&self, namespace: &str, key: &str, current: &str) -> Option<String> {
        match (namespace, key) {
            ("general", "approval_mode") => Some(
                match current {
                    "safe" => "danger",
                    "danger" => "safe",
                    _ => "safe",
                }
                .to_string(),
            ),
            ("general", "input_preset") => {
                let opts = ["custom", "lines", "box", "filled"];
                let i = opts.iter().position(|o| *o == current).unwrap_or(0);
                Some(opts[(i + 1) % opts.len()].to_string())
            }
            ("status", "status_spinner_style") => {
                let opts = [
                    "braille",
                    "triangle",
                    "pipe",
                    "kaomoji",
                    "typing",
                    "waveline",
                    "dots_text",
                    "progblock",
                ];
                let i = opts.iter().position(|o| *o == current).unwrap_or(0);
                Some(opts[(i + 1) % opts.len()].to_string())
            }
            ("status", "status_spinner_text") => {
                let opts = ["thinking", "pondering", "processing"];
                let i = opts.iter().position(|o| *o == current).unwrap_or(0);
                Some(opts[(i + 1) % opts.len()].to_string())
            }
            ("status", k) if k.starts_with("status_show_") || k == "status_spinner_text_rotate" => {
                Some(
                    match current {
                        "true" => "false",
                        _ => "true",
                    }
                    .to_string(),
                )
            }
            _ => None,
        }
    }

    // ── Migration ─────────────────────────────────────────────────────────

    /// One-time migration from legacy page values.
    /// Non-canonical fields (providers, tools, commands, compaction) are ignored.
    pub fn migrate_from_pages(pages: &[(String, crate::config::custom::CustomConfigPage)]) -> Self {
        use crate::config::custom::CustomConfigs;

        let temp = CustomConfigs {
            pages: pages.to_vec(),
            settings: None,
        };

        let approval = match temp.get_value("general", "approval_mode").as_str() {
            "safe" | "danger" => temp.get_value("general", "approval_mode"),
            _ => default_approval(),
        };
        let show_reasoning = temp.get_value("general", "show_thinking") == "true";
        let input_preset = match temp.get_value("general", "input_preset").as_str() {
            "custom" | "lines" | "box" | "filled" => {
                Some(temp.get_value("general", "input_preset"))
            }
            _ => None,
        };

        let ui = UiSettings {
            input: UiInputSettings {
                preset: input_preset,
                ..Default::default()
            },
            status_show_model: temp.get_value("status", "status_show_model") != "false",
            status_show_approval: temp.get_value("status", "status_show_approval") != "false",
            status_show_tokens_curr: temp.get_value("status", "status_show_tokens_curr") != "false",
            status_show_tokens_in: temp.get_value("status", "status_show_tokens_in") != "false",
            status_show_tokens_out: temp.get_value("status", "status_show_tokens_out") != "false",
            status_show_tokens_total: temp.get_value("status", "status_show_tokens_total")
                != "false",
            status_show_queue: temp.get_value("status", "status_show_queue") != "false",
            status_show_spinner: temp.get_value("status", "status_show_spinner") != "false",
            status_show_timer: temp.get_value("status", "status_show_timer") != "false",
            spinner_style: {
                let v = temp.get_value("status", "status_spinner_style");
                if v.is_empty() {
                    default_spinner_style()
                } else {
                    v
                }
            },
            spinner_text: {
                let v = temp.get_value("status", "status_spinner_text");
                if v.is_empty() {
                    default_spinner_text()
                } else {
                    v
                }
            },
            spinner_speed: temp
                .get_value("status", "status_spinner_speed")
                .parse::<u64>()
                .unwrap_or(0),
            spinner_text_rotate: temp.get_value("status", "status_spinner_text_rotate") != "false",
            spinner_text_speed: temp
                .get_value("status", "status_spinner_text_speed")
                .parse::<u64>()
                .unwrap_or(0),
            spinner_custom: temp.get_value("status", "status_spinner_text_custom"),
        };

        Self {
            inner: BoneSettings {
                version: 2,
                general: GeneralSettings {
                    approval,
                    show_reasoning,
                },
                ui,
                theme: ThemeSettings::default(),
                tools: EnablementSettings::default(),
                commands: EnablementSettings::default(),
                keymaps: KeymapSettings::default(),
                subagents: BTreeMap::new(),
                extensions: BTreeMap::new(),
            },
            revision: 0,
        }
    }

    // ── Internal UI get/set ──────────────────────────────────────────────

    fn get_ui(&self, key: &str) -> String {
        match key {
            "status_show_model" => self.inner.ui.status_show_model.to_string(),
            "status_show_approval" => self.inner.ui.status_show_approval.to_string(),
            "status_show_tokens_curr" => self.inner.ui.status_show_tokens_curr.to_string(),
            "status_show_tokens_in" => self.inner.ui.status_show_tokens_in.to_string(),
            "status_show_tokens_out" => self.inner.ui.status_show_tokens_out.to_string(),
            "status_show_tokens_total" => self.inner.ui.status_show_tokens_total.to_string(),
            "status_show_queue" => self.inner.ui.status_show_queue.to_string(),
            "status_show_spinner" => self.inner.ui.status_show_spinner.to_string(),
            "status_show_timer" => self.inner.ui.status_show_timer.to_string(),
            "status_spinner_style" => self.inner.ui.spinner_style.clone(),
            "status_spinner_text" => self.inner.ui.spinner_text.clone(),
            "status_spinner_speed" => self.inner.ui.spinner_speed.to_string(),
            "status_spinner_text_rotate" => self.inner.ui.spinner_text_rotate.to_string(),
            "status_spinner_text_speed" => self.inner.ui.spinner_text_speed.to_string(),
            "status_spinner_text_custom" => self.inner.ui.spinner_custom.clone(),
            _ => String::new(),
        }
    }

    fn set_ui(&mut self, key: &str, value: String) -> Result<(), SettingsError> {
        match key {
            "status_show_model"
            | "status_show_approval"
            | "status_show_tokens_curr"
            | "status_show_tokens_in"
            | "status_show_tokens_out"
            | "status_show_tokens_total"
            | "status_show_queue"
            | "status_show_spinner"
            | "status_show_timer"
            | "status_spinner_text_rotate" => {
                let val = parse_bool(&value)?;
                match key {
                    "status_show_model" => self.inner.ui.status_show_model = val,
                    "status_show_approval" => self.inner.ui.status_show_approval = val,
                    "status_show_tokens_curr" => self.inner.ui.status_show_tokens_curr = val,
                    "status_show_tokens_in" => self.inner.ui.status_show_tokens_in = val,
                    "status_show_tokens_out" => self.inner.ui.status_show_tokens_out = val,
                    "status_show_tokens_total" => self.inner.ui.status_show_tokens_total = val,
                    "status_show_queue" => self.inner.ui.status_show_queue = val,
                    "status_show_spinner" => self.inner.ui.status_show_spinner = val,
                    "status_show_timer" => self.inner.ui.status_show_timer = val,
                    "status_spinner_text_rotate" => self.inner.ui.spinner_text_rotate = val,
                    _ => {}
                }
                Ok(())
            }
            "status_spinner_style" => {
                if value.is_empty() {
                    return Err(SettingsError::Validation(
                        "status_spinner_style must not be empty".into(),
                    ));
                }
                self.inner.ui.spinner_style = value;
                Ok(())
            }
            "status_spinner_text" => {
                if value.is_empty() {
                    return Err(SettingsError::Validation(
                        "status_spinner_text must not be empty".into(),
                    ));
                }
                self.inner.ui.spinner_text = value;
                Ok(())
            }
            "status_spinner_text_custom" => {
                self.inner.ui.spinner_custom = value;
                Ok(())
            }
            "status_spinner_speed" | "status_spinner_text_speed" => {
                let n = value.parse::<u64>().map_err(|_| {
                    SettingsError::Validation(format!(
                        "{key}: expected non-negative integer, got {value:?}"
                    ))
                })?;
                if key == "status_spinner_speed" {
                    self.inner.ui.spinner_speed = n;
                } else {
                    self.inner.ui.spinner_text_speed = n;
                }
                Ok(())
            }
            _ => Err(SettingsError::Validation(format!(
                "unknown status key: {key}"
            ))),
        }
    }
}

// ── Validation ───────────────────────────────────────────────────────────────

fn validate_settings(settings: &BoneSettings) -> Result<(), SettingsError> {
    if !matches!(settings.version, 1 | 2) {
        return Err(SettingsError::BadVersion(settings.version));
    }
    validate_general(&settings.general)?;
    validate_theme(&settings.theme)?;
    validate_keymaps(&settings.keymaps)?;
    validate_subagents(&settings.subagents)
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
    if agent.max_concurrency == Some(0) {
        return Err(SettingsError::Validation(format!(
            "subagents.{name}.max_concurrency must be at least 1"
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

fn validate_theme(theme: &ThemeSettings) -> Result<(), SettingsError> {
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

fn parse_bool(s: &str) -> Result<bool, SettingsError> {
    match s {
        "true" | "1" | "yes" => Ok(true),
        "false" | "0" | "no" => Ok(false),
        _ => Err(SettingsError::Validation(format!(
            "expected boolean, got {s:?}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "bone-settings-{name}-{}-{}.yaml",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn validates_version_unknown_keys_and_values() {
        let bad_version = "version: 2\n";
        let parsed: BoneSettings = serde_yaml::from_str(bad_version).unwrap();
        assert_eq!(parsed.version, 2);

        assert!(serde_yaml::from_str::<BoneSettings>("version: 1\nunknown: true\n").is_err());
        assert!(validate_approval("prompt").is_err());
        assert!(parse_bool("maybe").is_err());
        assert!(
            validate_keymaps(&KeymapSettings {
                bindings: vec![KeyBinding {
                    key: String::new(),
                    action: "quit".into(),
                }],
            })
            .is_err()
        );
        assert!(
            validate_keymaps(&KeymapSettings {
                bindings: vec![
                    KeyBinding {
                        key: "<C-p>".into(),
                        action: "one".into(),
                    },
                    KeyBinding {
                        key: "<C-p>".into(),
                        action: "two".into(),
                    },
                ],
            })
            .is_err()
        );
    }

    #[test]
    fn atomically_persists_and_loads_values_only_yaml() {
        let path = temp_path("roundtrip");
        let mut settings = Settings::defaults();
        settings.inner.general.approval = "danger".into();
        settings.inner.ui.input.preset = Some("box".into());
        settings.inner.subagents.insert(
            "researcher".into(),
            SubagentSettings {
                description: "Investigates the codebase".into(),
                system_prompt: Some("Report file and line references.".into()),
                ..Default::default()
            },
        );
        settings.save_path(&path).unwrap();
        settings.inner.general.approval = "safe".into();
        settings.save_path(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }

        let loaded = Settings::load_path(&path).unwrap().unwrap();
        assert_eq!(loaded.inner.general.approval, "safe");
        assert_eq!(loaded.inner.ui.input.preset.as_deref(), Some("box"));
        assert!(loaded.inner.subagents.is_empty());
        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("version: 2"));
        assert!(!raw.contains("subagents:"));
        assert!(!raw.contains("Report file and line references."));
        assert!(!raw.contains("label:"));
        assert!(!raw.contains("null"));
        assert!(!raw.contains("show_reasoning"));
        assert!(!raw.contains("theme:"));
        assert!(
            raw.lines().count() < 10,
            "config should stay sparse:\n{raw}"
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn verbose_input_round_trips_to_sparse_output() {
        let path = temp_path("verbose-roundtrip");
        let mut original = Settings::defaults();
        original.inner.general.approval = "danger".into();
        original.inner.ui.input.prefix = Some("λ ".into());
        original.inner.ui.status_show_timer = false;
        let verbose = serde_yaml::to_string(original.resolved()).unwrap();
        assert!(verbose.contains("show_reasoning: false"));
        assert!(verbose.contains("theme:"));
        fs::write(&path, verbose).unwrap();

        let loaded = Settings::load_path(&path).unwrap().unwrap();
        loaded.save_path(&path).unwrap();
        let reloaded = Settings::load_path(&path).unwrap().unwrap();
        assert_eq!(reloaded.inner.general.approval, "danger");
        assert_eq!(reloaded.inner.ui.input.prefix.as_deref(), Some("λ "));
        assert!(!reloaded.inner.ui.status_show_timer);
        let sparse = fs::read_to_string(&path).unwrap();
        assert!(!sparse.contains("show_reasoning"));
        assert!(!sparse.contains("theme:"));
        assert!(sparse.contains("approval: danger"));
        assert!(sparse.contains("status_show_timer: false"));

        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(lock_path_for(&path));
    }

    #[test]
    fn field_updates_reload_latest_document_before_saving() {
        let path = temp_path("concurrent-updates");
        let initial = Settings::defaults();
        initial.save_path(&path).unwrap();
        let mut first = Settings::load_path(&path).unwrap().unwrap();
        let mut stale = Settings::load_path(&path).unwrap().unwrap();

        first
            .set_path_at("general.show_reasoning", true.into(), &path)
            .unwrap();
        stale
            .set_path_at("ui.status_show_timer", false.into(), &path)
            .unwrap();

        let loaded = Settings::load_path(&path).unwrap().unwrap();
        assert!(loaded.inner.general.show_reasoning);
        assert!(!loaded.inner.ui.status_show_timer);
        assert!(stale.inner.general.show_reasoning);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_invalid_files_without_overwriting_them() {
        let malformed = temp_path("malformed");
        fs::write(&malformed, "version: 1\ngeneral: [\n").unwrap();
        assert!(matches!(
            Settings::load_path(&malformed),
            Err(SettingsError::Parse(_))
        ));
        assert_eq!(
            fs::read_to_string(&malformed).unwrap(),
            "version: 1\ngeneral: [\n"
        );

        let bad_version = temp_path("bad-version");
        fs::write(&bad_version, "version: 3\n").unwrap();
        assert!(matches!(
            Settings::load_path(&bad_version),
            Err(SettingsError::BadVersion(3))
        ));

        let _ = fs::remove_file(malformed);
        let _ = fs::remove_file(bad_version);
    }

    #[test]
    fn migrates_supported_legacy_page_values() {
        use crate::config::custom::{ConfigField, ConfigFieldType, CustomConfigPage};

        let field = |key: &str, value: serde_yaml::Value| ConfigField {
            key: key.into(),
            label: None,
            field_type: ConfigFieldType::String,
            options: Vec::new(),
            default: None,
            value: Some(value),
        };
        let pages = vec![
            (
                "general".into(),
                CustomConfigPage {
                    title: "General".into(),
                    fields: vec![
                        field("approval_mode", "danger".into()),
                        field("show_thinking", true.into()),
                        field("input_preset", "box".into()),
                    ],
                },
            ),
            (
                "status".into(),
                CustomConfigPage {
                    title: "Status".into(),
                    fields: vec![field("status_show_timer", false.into())],
                },
            ),
        ];

        let migrated = Settings::migrate_from_pages(&pages);
        assert_eq!(migrated.inner.general.approval, "danger");
        assert!(migrated.inner.general.show_reasoning);
        assert_eq!(migrated.inner.ui.input.preset.as_deref(), Some("box"));
        assert!(!migrated.inner.ui.status_show_timer);
    }

    // ── Cross-process lock tests ──────────────────────────────────────────

    #[test]
    fn lock_path_is_sibling_with_dot_lock_suffix() {
        let p = Path::new("/home/user/.bone-rust/config.yaml");
        let lock = lock_path_for(p);
        assert_eq!(
            lock,
            PathBuf::from("/home/user/.bone-rust/config.yaml.lock")
        );

        // Works with just a filename too.
        let bare = Path::new("config.yaml");
        assert_eq!(lock_path_for(bare), PathBuf::from("config.yaml.lock"));
    }

    #[test]
    fn lock_errors_include_operation_path_and_os_error() {
        let path = PathBuf::from("exact/config.yaml.lock");
        for (operation, expected) in [
            (
                "create parent directory for",
                "cannot create parent directory for settings lock exact/config.yaml.lock: denied",
            ),
            (
                "open",
                "cannot open settings lock exact/config.yaml.lock: denied",
            ),
            (
                "acquire",
                "cannot acquire settings lock exact/config.yaml.lock: denied",
            ),
        ] {
            let error = SettingsError::Lock {
                operation,
                path: path.clone(),
                source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
            };
            assert_eq!(error.to_string(), expected);
        }
        assert_eq!(
            SettingsError::Io(std::io::Error::other("plain")).to_string(),
            "settings I/O error: plain"
        );
    }

    #[test]
    fn save_creates_lock_file() {
        let path = temp_path("lock-create");
        Settings::defaults().save_path(&path).unwrap();
        assert!(lock_path_for(&path).exists());
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(lock_path_for(&path));
    }
}

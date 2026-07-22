//! Legacy custom-page compatibility types and helpers.
//!
//! The daemon's `ConfigStore` is the live authority. These APIs remain for
//! migration, setup compatibility, and external callers that still construct
//! page-format values; legacy page files are not live runtime authorities.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::settings::Settings;
use super::{bone_dir, load_yaml, seed_file_forced, seed_file_if_missing};

// ── Schema types ────────────────────────────────────────────────────────────

/// A single field definition in a custom config page.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ConfigField {
    pub key: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default, rename = "type")]
    pub field_type: ConfigFieldType,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    #[serde(default)]
    pub default: Option<serde_yaml::Value>,
    /// Legacy page value retained for migration and compatibility callers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_yaml::Value>,
}

/// Supported field types for custom config values.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ConfigFieldType {
    #[default]
    String,
    Number,
    Bool,
    Enum,
    /// A provider entry (label, base_url, model, api_key, endpoint, handler).
    /// Stored as a nested YAML map in `value`; use `get_provider_entry()` to read it.
    Provider,
}

/// A parsed custom config page (from one YAML file).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CustomConfigPage {
    pub title: String,
    pub fields: Vec<ConfigField>,
}

/// Deny-list format for tools/commands pages.
/// The page is built dynamically from the filesystem; the YAML only stores disabled names.
#[derive(Debug, Clone, Deserialize, Serialize)]
struct DenyListPage {
    title: String,
    #[serde(default)]
    disabled: Vec<String>,
}

/// Tool names that are built-in (native Rust) and always appear on the tools page.
#[cfg(test)]
const NATIVE_TOOLS: &[&str] = &["shell", "read_file", "write_file", "edit_file"];

/// All loaded custom pages, keyed by filename stem.
#[derive(Debug, Clone, Default)]
pub struct CustomConfigs {
    /// filename stem -> page
    pub pages: Vec<(String, CustomConfigPage)>,
    /// Canonical settings, loaded from `~/.bone-rust/config.yaml`.
    /// `None` when the file has not been loaded yet (or does not exist).
    pub settings: Option<Settings>,
}

// ── Paths ───────────────────────────────────────────────────────────────────

pub fn config_dir() -> PathBuf {
    bone_dir().join("config")
}

/// Report a malformed config page once with the specific parse error.
#[cfg(test)]
fn warn_parse_failure(detail: &str) {
    crate::ext::ctx::runtime_warn_once(format!("bone: warning: {detail}"));
}

fn write_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(contents)?;
    if let Ok(metadata) = std::fs::metadata(path) {
        temporary
            .as_file()
            .set_permissions(metadata.permissions())?;
    }
    temporary.as_file_mut().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .map(|_| ())
}

// ── Built-in seed pages ────────────────────────────────────────────────────

const GENERAL_YAML: &str = include_str!("pages/general.yaml");
const TOOLS_YAML: &str = include_str!("pages/tools.yaml");
const PROVIDERS_YAML: &str = include_str!("pages/providers.yaml");
const STATUS_YAML: &str = include_str!("pages/status.yaml");
const COMMANDS_YAML: &str = include_str!("pages/commands.yaml");

/// `(filename, embedded contents)` for every built-in config page.
const BUILTIN_PAGES: &[(&str, &str)] = &[
    ("general.yaml", GENERAL_YAML),
    ("tools.yaml", TOOLS_YAML),
    ("status.yaml", STATUS_YAML),
    ("providers.yaml", PROVIDERS_YAML),
    ("commands.yaml", COMMANDS_YAML),
];

/// Compatibility helper for seeding legacy page fixtures. Live startup does
/// not call this function. `allow` filters which pages are written (`None` =
/// all); `force` controls whether existing fixture files are overwritten.
pub fn seed_builtin_pages(allow: Option<&std::collections::HashSet<String>>, force: bool) {
    let dir = config_dir();
    let _ = std::fs::create_dir_all(&dir);
    for (name, content) in BUILTIN_PAGES {
        if let Some(allow) = allow
            && !allow.contains(*name)
        {
            continue;
        }
        let path = dir.join(name);
        if force {
            seed_file_forced(&path, content);
        } else {
            seed_file_if_missing(&path, content);
        }
    }
}

// ── Load / save ─────────────────────────────────────────────────────────────

impl CustomConfigs {
    /// Scan `~/.bone-rust/config/` for `*.yaml` files and load them.
    #[cfg(test)]
    pub(crate) fn load() -> Self {
        let dir = config_dir();
        let mut configs = CustomConfigs::default();

        if !dir.is_dir() {
            // No config directory yet — try loading settings anyway for migration.
            configs.settings = Self::load_or_migrate_settings(&configs.pages);
            return configs;
        }

        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => {
                configs.settings = Self::load_or_migrate_settings(&configs.pages);
                return configs;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            if stem.is_empty() {
                continue;
            }
            // Tools and commands use deny-list format; everything else uses field format.
            let is_denylist = stem == "tools" || stem == "commands";
            if is_denylist {
                match load_yaml::<DenyListPage>(&path) {
                    Ok(deny) => {
                        configs.pages.push((
                            stem,
                            CustomConfigPage {
                                title: deny.title,
                                fields: Vec::new(),
                            },
                        ));
                    }
                    Err(_) => {
                        // Old field-based format – will be migrated by
                        // rebuild_denylist_pages().
                        match load_yaml::<CustomConfigPage>(&path) {
                            Ok(page) => configs.pages.push((stem, page)),
                            Err(e) => warn_parse_failure(&e),
                        }
                    }
                }
            } else {
                match load_yaml::<CustomConfigPage>(&path) {
                    Ok(page) => configs.pages.push((stem, page)),
                    Err(e) => warn_parse_failure(&e),
                }
            }
        }

        configs.pages.sort_by(|a, b| a.0.cmp(&b.0));
        configs.rebuild_denylist_pages();

        // Load or migrate canonical settings after pages are populated.
        configs.settings = Self::load_or_migrate_settings(&configs.pages);

        configs
    }

    /// Load canonical settings from `config.yaml`, or migrate from pages if
    /// the file does not yet exist.  This is deliberately **not** recursive —
    /// it does not call `load()` again, so there is no infinite loop.
    #[cfg(test)]
    fn load_or_migrate_settings(pages: &[(String, CustomConfigPage)]) -> Option<Settings> {
        match Settings::load() {
            Ok(Some(s)) => Some(s),
            Ok(None) => {
                // First boot: migrate from legacy pages and persist.
                let s = Settings::migrate_from_pages(pages);
                if let Err(e) = s.save() {
                    crate::ext::ctx::runtime_warn(format!(
                        "bone: warning: could not write canonical settings: {e}"
                    ));
                }
                Some(s)
            }
            Err(e) => {
                crate::ext::ctx::runtime_warn_once(format!(
                    "bone: error: could not load canonical settings: {e}"
                ));
                // Never fall back to legacy values after config.yaml exists. Keep
                // validated defaults active while leaving the invalid file untouched.
                Some(Settings::defaults())
            }
        }
    }

    /// Save a single page back to its YAML file.
    fn save_page(&self, namespace: &str) -> bool {
        if let Some(page) = self.page_ref(namespace) {
            let path = config_dir().join(format!("{namespace}.yaml"));
            let yaml = match serde_yaml::to_string(page) {
                Ok(y) => y,
                Err(_) => return false,
            };
            let permissions = std::fs::metadata(&path).ok().map(|meta| meta.permissions());
            return crate::tools::write_atomic::write_atomic_sync(
                &path,
                yaml.as_bytes(),
                permissions,
            )
            .is_ok();
        }
        false
    }

    /// Persist the named page; if saving fails, revert the field to its prior value
    /// so the UI does not show a change that was never written to disk.
    fn save_or_revert(&mut self, namespace: &str, key: &str, old_value: Option<serde_yaml::Value>) {
        let page_path = config_dir().join(format!("{namespace}.yaml"));
        if page_path.exists()
            && !self.save_page(namespace)
            && let Some(page) = self.page_mut(namespace)
            && let Some(field) = page.fields.iter_mut().find(|f| f.key == key)
        {
            field.value = old_value;
        }
    }
    // ── Deny-list page helpers ──────────────────────────────────────────────

    /// Scan a Lua directory for .lua file stems.
    #[cfg(test)]
    fn scan_lua_dir(dir: &std::path::Path) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        if !dir.is_dir() {
            return names;
        }
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("lua")
                    && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
                {
                    names.push(stem.to_string());
                }
            }
        }
        names.sort();
        names
    }

    /// Rebuild the tools and commands pages from the filesystem + deny-list.
    #[cfg(test)]
    fn rebuild_denylist_pages(&mut self) {
        let lua_tools = Self::scan_lua_dir(&bone_dir().join("lua").join("tools"));
        let lua_commands = Self::scan_lua_dir(&bone_dir().join("lua").join("commands"));

        // Rebuild tools page
        if let Some(pos) = self.page_index("tools") {
            let (disabled, title) = self.read_denylist("tools");
            let mut fields: Vec<ConfigField> = Vec::new();
            let disabled_set: std::collections::HashSet<&str> =
                disabled.iter().map(|s| s.as_str()).collect();

            // Native tools first
            for name in NATIVE_TOOLS {
                let is_disabled = disabled_set.contains(name);
                fields.push(ConfigField {
                    key: name.to_string(),
                    label: Some(name.to_string()),
                    field_type: ConfigFieldType::Bool,
                    options: Vec::new(),
                    default: Some(serde_yaml::Value::Bool(true)),
                    value: if is_disabled {
                        Some(serde_yaml::Value::Bool(false))
                    } else {
                        None
                    },
                });
            }

            // Lua tools
            for name in &lua_tools {
                if NATIVE_TOOLS.contains(&name.as_str()) {
                    continue;
                }
                let is_disabled = disabled_set.contains(name.as_str());
                fields.push(ConfigField {
                    key: name.clone(),
                    label: Some(name.clone()),
                    field_type: ConfigFieldType::Bool,
                    options: Vec::new(),
                    default: Some(serde_yaml::Value::Bool(true)),
                    value: if is_disabled {
                        Some(serde_yaml::Value::Bool(false))
                    } else {
                        None
                    },
                });
            }

            self.pages[pos].1 = CustomConfigPage { title, fields };
        }

        // Rebuild commands page
        if let Some(pos) = self.page_index("commands") {
            let (disabled, title) = self.read_denylist("commands");
            let mut fields: Vec<ConfigField> = Vec::new();
            let disabled_set: std::collections::HashSet<&str> =
                disabled.iter().map(|s| s.as_str()).collect();

            for name in &lua_commands {
                // Protected built-ins (e.g. /config) can't actually be
                // disabled — the dispatch bypass and `is_protected_builtin`
                // guard run before the deny-list branch, so a toggle would be
                // a silent no-op. Don't offer one.
                if crate::commands::is_protected_builtin(name.as_str()) {
                    continue;
                }
                let is_disabled = disabled_set.contains(name.as_str());
                fields.push(ConfigField {
                    key: name.clone(),
                    label: Some(name.clone()),
                    field_type: ConfigFieldType::Bool,
                    options: Vec::new(),
                    default: Some(serde_yaml::Value::Bool(true)),
                    value: if is_disabled {
                        Some(serde_yaml::Value::Bool(false))
                    } else {
                        None
                    },
                });
            }

            self.pages[pos].1 = CustomConfigPage { title, fields };
        }
    }

    /// Read the deny-list from a YAML file, migrating old format if needed.
    fn read_denylist(&self, namespace: &str) -> (Vec<String>, String) {
        let path = config_dir().join(format!("{namespace}.yaml"));
        if !path.exists() {
            return (Vec::new(), String::new());
        }

        // Try old field-based format first and migrate
        if let Ok(page) = load_yaml::<CustomConfigPage>(&path) {
            if !page.fields.is_empty() {
                let disabled: Vec<String> = page
                    .fields
                    .iter()
                    .filter(|f| {
                        f.value
                            .as_ref()
                            .map(|v| v == &serde_yaml::Value::Bool(false))
                            .unwrap_or(false)
                    })
                    .map(|f| f.key.clone())
                    .collect();

                // Write new deny-list format
                let new_page = DenyListPage {
                    title: page.title.clone(),
                    disabled: disabled.clone(),
                };
                if let Ok(yaml) = serde_yaml::to_string(&new_page) {
                    let _ = std::fs::write(&path, yaml);
                }

                return (disabled, page.title);
            }
            // Empty fields — treat as new format with empty disabled
            return (Vec::new(), page.title);
        }

        // Try new deny-list format
        if let Ok(deny) = load_yaml::<DenyListPage>(&path) {
            return (deny.disabled, deny.title);
        }

        (Vec::new(), String::new())
    }

    /// Save the deny-list for a tools/commands page.
    /// Returns true if the write succeeded.
    fn save_denylist(&self, namespace: &str, disabled: &[String], title: &str) -> bool {
        let path = config_dir().join(format!("{namespace}.yaml"));
        let page = DenyListPage {
            title: title.to_string(),
            disabled: disabled.to_vec(),
        };
        let yaml = match serde_yaml::to_string(&page) {
            Ok(y) => y,
            Err(_) => return false,
        };
        write_atomic(&path, yaml.as_bytes()).is_ok()
    }

    fn page_ref(&self, namespace: &str) -> Option<&CustomConfigPage> {
        self.pages
            .iter()
            .find(|(ns, _)| ns == namespace)
            .map(|(_, page)| page)
    }

    fn page_mut(&mut self, namespace: &str) -> Option<&mut CustomConfigPage> {
        self.pages
            .iter_mut()
            .find(|(ns, _)| ns == namespace)
            .map(|(_, page)| page)
    }

    fn page_index(&self, namespace: &str) -> Option<usize> {
        self.pages.iter().position(|(ns, _)| ns == namespace)
    }

    /// Get all enabled names from a namespace page.
    fn enabled_names(&self, namespace: &str) -> Vec<String> {
        let pos = match self.page_index(namespace) {
            Some(p) => p,
            None => return Vec::new(),
        };
        let page = &self.pages[pos].1;
        page.fields
            .iter()
            .filter(|f| {
                let val = self.get_value(namespace, &f.key);
                val == "true" || val.is_empty()
            })
            .map(|f| f.key.clone())
            .collect()
    }

    /// Get all enabled tool names from the "tools" page.
    pub fn enabled_tool_names(&self) -> Vec<String> {
        self.enabled_names("tools")
    }

    /// Get the persisted deny-list for tools or commands.
    pub fn disabled_names(&self, namespace: &str) -> Vec<String> {
        self.read_denylist(namespace).0
    }

    /// Get the display value for a field, falling back to the default.
    /// Routes canonical keys (general approval/show_thinking/input_preset and
    /// all status fields) through [`Settings`] when available.
    pub fn get_value(&self, namespace: &str, key: &str) -> String {
        // Route canonical keys through settings.
        if Settings::is_canonical(namespace, key)
            && let Some(settings) = self.settings.as_ref()
        {
            return settings.get_value(namespace, key);
        }
        let Some(page) = self.page_ref(namespace) else {
            return String::new();
        };
        let field = page.fields.iter().find(|f| f.key == key);
        let Some(field) = field else {
            return String::new();
        };
        let val = field.value.as_ref().or(field.default.as_ref());
        match val {
            Some(serde_yaml::Value::String(s)) => s.clone(),
            Some(serde_yaml::Value::Number(n)) => n.to_string(),
            Some(serde_yaml::Value::Bool(b)) => b.to_string(),
            Some(other) => format!("{other:?}"),
            None => String::new(),
        }
    }

    /// Set a value and persist immediately. Canonical settings return validation
    /// and persistence failures to the caller; legacy page writes report a
    /// generic I/O failure when the save does not succeed.
    pub fn try_set_value(
        &mut self,
        namespace: &str,
        key: &str,
        value: String,
    ) -> Result<(), String> {
        if Settings::is_canonical(namespace, key)
            && let Some(settings) = self.settings.as_mut()
        {
            return settings
                .set_value(namespace, key, value)
                .map_err(|e| e.to_string());
        }

        self.set_legacy_value(namespace, key, value)
    }

    /// Compatibility wrapper for callers that cannot surface errors.
    pub fn set_value(&mut self, namespace: &str, key: &str, value: String) {
        if let Err(e) = self.try_set_value(namespace, key, value) {
            crate::ext::ctx::runtime_warn(format!("bone: warning: set_value failed: {e}"));
        }
    }

    fn set_legacy_value(
        &mut self,
        namespace: &str,
        key: &str,
        value: String,
    ) -> Result<(), String> {
        // Deny-list pages: update the deny-list YAML directly.
        if namespace == "tools" || namespace == "commands" {
            let (mut disabled, title) = self.read_denylist(namespace);
            if value == "false" {
                if !disabled.contains(&key.to_string()) {
                    disabled.push(key.to_string());
                }
            } else {
                disabled.retain(|d| d != key);
            }
            if !self.save_denylist(namespace, &disabled, &title) {
                return Err(format!("could not save {namespace}.yaml"));
            }
            // Update in-memory field for immediate UI feedback.
            if let Some(page) = self.page_mut(namespace)
                && let Some(field) = page.fields.iter_mut().find(|f| f.key == key)
            {
                let yaml_val = match value.as_str() {
                    "true" => serde_yaml::Value::Bool(true),
                    "false" => serde_yaml::Value::Bool(false),
                    _ => serde_yaml::Value::String(value),
                };
                field.value = Some(yaml_val);
            }
            return Ok(());
        }

        let Some(page) = self.page_mut(namespace) else {
            return Err(format!("unknown config namespace: {namespace}"));
        };
        let field = page.fields.iter_mut().find(|f| f.key == key);
        let Some(field) = field else {
            return Err(format!("unknown config field: {namespace}.{key}"));
        };
        let yaml_val = match field.field_type {
            ConfigFieldType::Bool => match value.as_str() {
                "true" => serde_yaml::Value::Bool(true),
                "false" => serde_yaml::Value::Bool(false),
                _ => serde_yaml::Value::String(value),
            },
            ConfigFieldType::Number => value
                .parse::<serde_yaml::Number>()
                .map(serde_yaml::Value::Number)
                .unwrap_or_else(|_| serde_yaml::Value::String(value.clone())),
            _ => serde_yaml::Value::String(value),
        };
        let old_value = field.value.clone();
        field.value = Some(yaml_val);
        if !self.save_page(namespace) {
            if let Some(page) = self.page_mut(namespace)
                && let Some(field) = page.fields.iter_mut().find(|f| f.key == key)
            {
                field.value = old_value;
            }
            return Err(format!("could not save {namespace}.yaml"));
        }
        Ok(())
    }

    /// Find a field definition by namespace and key.
    pub fn find_field(&self, namespace: &str, key: &str) -> Option<&ConfigField> {
        let page = self.page_ref(namespace)?;
        page.fields.iter().find(|f| f.key == key)
    }

    /// Cycle to the next option for a bool or enum field.
    /// Returns the new value string.
    pub fn cycle_field(&self, namespace: &str, key: &str, current: &str) -> Option<String> {
        let field = self.find_field(namespace, key)?;
        match field.field_type {
            ConfigFieldType::Bool => {
                let next = match current {
                    "true" => "false",
                    _ => "true",
                };
                Some(next.to_string())
            }
            ConfigFieldType::Enum => {
                let options = &field.options;
                if options.is_empty() {
                    return None;
                }
                let idx = options.iter().position(|o| o == current).unwrap_or(0);
                let next = (idx + 1) % options.len();
                Some(options[next].clone())
            }
            _ => None,
        }
    }

    // ── Provider helpers ────────────────────────────────────────────────────

    /// Get a provider entry from a provider field's value.
    pub fn get_provider_entry(
        &self,
        namespace: &str,
        key: &str,
    ) -> Option<crate::config::ProviderEntry> {
        let field = self.find_field(namespace, key)?;
        let val = field.value.as_ref()?;
        crate::config::ProviderEntry::from_nested(val)
    }

    /// Set a provider entry as a nested YAML map in the field's value.
    pub fn set_provider_entry(
        &mut self,
        namespace: &str,
        key: &str,
        entry: &crate::config::ProviderEntry,
    ) {
        let Some(page) = self.page_mut(namespace) else {
            return;
        };
        let Some(field) = page.fields.iter_mut().find(|f| f.key == key) else {
            return;
        };
        let old_value = field.value.clone();
        if let Ok(nested) = serde_yaml::to_value(entry) {
            field.value = Some(nested);
        }
        self.save_or_revert(namespace, key, old_value);
    }

    /// Insert or replace a provider and persist the providers page.
    #[cfg(test)]
    pub(crate) fn upsert_provider_entry(
        &mut self,
        key: &str,
        entry: &crate::config::ProviderEntry,
    ) -> Result<(), String> {
        if key.is_empty()
            || key.starts_with('_')
            || !key
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
        {
            return Err(format!("invalid provider id: {key:?}"));
        }
        let nested = serde_yaml::to_value(entry).map_err(|e| e.to_string())?;
        let page = self
            .page_mut("providers")
            .ok_or_else(|| "providers config page is missing".to_string())?;
        let backup = page.fields.clone();
        if let Some(field) = page.fields.iter_mut().find(|field| field.key == key) {
            field.label = Some(entry.label.clone());
            field.field_type = ConfigFieldType::Provider;
            field.value = Some(nested);
        } else {
            page.fields.push(ConfigField {
                key: key.to_string(),
                label: Some(entry.label.clone()),
                field_type: ConfigFieldType::Provider,
                options: Vec::new(),
                default: None,
                value: Some(nested),
            });
        }
        if self.save_page("providers") {
            Ok(())
        } else {
            self.page_mut("providers").unwrap().fields = backup;
            Err("could not save providers.yaml".into())
        }
    }

    /// Derive a ProvidersConfig from the providers page fields.
    pub fn derive_providers_config(&self) -> crate::config::ProvidersConfig {
        let mut cfg = crate::config::ProvidersConfig::default();
        let Some(page) = self.page_ref("providers") else {
            return cfg;
        };
        for field in &page.fields {
            if field.key == "_last_provider" {
                cfg.last_provider = self.get_value("providers", &field.key);
                continue;
            }
            if let Some(entry) = self.get_provider_entry("providers", &field.key) {
                cfg.providers.insert(field.key.clone(), entry);
            }
        }
        cfg
    }

    /// Get the last used provider ID.
    pub fn get_last_provider(&self) -> String {
        self.get_value("providers", "_last_provider")
    }

    /// Set the last used provider ID.
    pub fn try_set_last_provider(&mut self, id: &str) -> Result<(), String> {
        self.try_set_value("providers", "_last_provider", id.to_string())
    }

    /// Compatibility wrapper for callers that cannot surface persistence errors.
    pub fn set_last_provider(&mut self, id: &str) {
        if let Err(error) = self.try_set_last_provider(id) {
            crate::ext::ctx::runtime_warn(format!(
                "bone: warning: could not save active provider: {error}"
            ));
        }
    }
}

#[cfg(test)]
#[path = "custom_tests.rs"]
mod custom_tests;

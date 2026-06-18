//! Custom user-defined config pages loaded from `~/.bone-rust/config/*.yaml`.
//!
//! Each page file (e.g. `general.yaml`, `tools.yaml`) contains both the field
//! schema *and* the current values. No separate values file is needed.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{UserConfig, bone_dir, load_yaml, seed_file_if_missing};

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
    /// Current runtime value, stored directly in the page YAML.
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
const NATIVE_TOOLS: &[&str] = &["shell", "read_file", "write_file", "edit_file"];

/// All loaded custom pages, keyed by filename stem.
#[derive(Debug, Clone, Default)]
pub struct CustomConfigs {
    /// filename stem -> page
    pub pages: Vec<(String, CustomConfigPage)>,
}

// ── Paths ───────────────────────────────────────────────────────────────────

pub fn config_dir() -> PathBuf {
    bone_dir().join("config")
}

// ── Built-in seed pages ────────────────────────────────────────────────────

const GENERAL_YAML: &str = include_str!("pages/general.yaml");
const TOOLS_YAML: &str = include_str!("pages/tools.yaml");
const PROVIDERS_YAML: &str = include_str!("pages/providers.yaml");
const STATUS_YAML: &str = include_str!("pages/status.yaml");
const COMMANDS_YAML: &str = include_str!("pages/commands.yaml");

/// Seed built-in config pages into `~/.bone-rust/config/` if missing.
pub fn seed_builtin_pages() {
    let dir = config_dir();
    let _ = std::fs::create_dir_all(&dir);
    seed_file_if_missing(&dir.join("general.yaml"), GENERAL_YAML);
    seed_file_if_missing(&dir.join("tools.yaml"), TOOLS_YAML);
    seed_file_if_missing(&dir.join("status.yaml"), STATUS_YAML);
    seed_file_if_missing(&dir.join("providers.yaml"), PROVIDERS_YAML);
    seed_file_if_missing(&dir.join("commands.yaml"), COMMANDS_YAML);
}

// ── Load / save ─────────────────────────────────────────────────────────────

impl CustomConfigs {
    /// Scan `~/.bone-rust/config/` for `*.yaml` files and load them.
    pub fn load() -> Self {
        migrate_old_values_file();
        migrate_status_values_from_general();
        migrate_providers_file();

        let dir = config_dir();
        let mut configs = CustomConfigs::default();

        if !dir.is_dir() {
            return configs;
        }

        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => return configs,
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
                if let Some(deny) = load_yaml::<DenyListPage>(&path) {
                    configs.pages.push((
                        stem,
                        CustomConfigPage {
                            title: deny.title,
                            fields: Vec::new(),
                        },
                    ));
                } else if let Some(page) = load_yaml::<CustomConfigPage>(&path) {
                    // Old format — will be migrated by rebuild_denylist_pages().
                    configs.pages.push((stem, page));
                } else {
                    eprintln!("bone: warning: failed to parse {}", path.display());
                }
            } else if let Some(page) = load_yaml::<CustomConfigPage>(&path) {
                configs.pages.push((stem, page));
            } else {
                eprintln!("bone: warning: failed to parse {}", path.display());
            }
        }

        configs.pages.sort_by(|a, b| a.0.cmp(&b.0));
        configs.rebuild_denylist_pages();
        configs
    }

    /// Save a single page back to its YAML file.
    fn save_page(&self, namespace: &str) -> bool {
        if let Some(page) = self.page_ref(namespace) {
            let path = config_dir().join(format!("{namespace}.yaml"));
            let yaml = match serde_yaml::to_string(page) {
                Ok(y) => y,
                Err(_) => return false,
            };
            return std::fs::write(path, yaml).is_ok();
        }
        false
    }

    // ── Deny-list page helpers ──────────────────────────────────────────────

    /// Scan a Lua directory for .lua file stems.
    fn scan_lua_dir(dir: &std::path::Path) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        if !dir.is_dir() {
            return names;
        }
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("lua") {
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        names.push(stem.to_string());
                    }
                }
            }
        }
        names.sort();
        names
    }

    /// Rebuild the tools and commands pages from the filesystem + deny-list.
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
        if let Some(page) = load_yaml::<CustomConfigPage>(&path) {
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
        if let Some(deny) = load_yaml::<DenyListPage>(&path) {
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
        std::fs::write(path, yaml).is_ok()
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

    /// Get all enabled command names from the "commands" page.
    pub fn enabled_command_names(&self) -> Vec<String> {
        self.enabled_names("commands")
    }

    /// Get the display value for a field, falling back to the default.
    pub fn get_value(&self, namespace: &str, key: &str) -> String {
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

    /// Set a value and persist immediately to the page YAML.
    pub fn set_value(&mut self, namespace: &str, key: &str, value: String) {
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
            if self.save_denylist(namespace, &disabled, &title) {
                // Update in-memory field for immediate UI feedback.
                if let Some(page) = self.page_mut(namespace) {
                    if let Some(field) = page.fields.iter_mut().find(|f| f.key == key) {
                        let yaml_val = match value.as_str() {
                            "true" => serde_yaml::Value::Bool(true),
                            "false" => serde_yaml::Value::Bool(false),
                            _ => serde_yaml::Value::String(value),
                        };
                        field.value = Some(yaml_val);
                    }
                }
            }
            return;
        }

        let Some(page) = self.page_mut(namespace) else {
            return;
        };
        let field = page.fields.iter_mut().find(|f| f.key == key);
        let Some(field) = field else {
            return;
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
        // Only persist if the page file actually exists on disk.
        // Pages that exist only in memory (e.g. test fixtures) must not
        // leak to the user's config directory.
        let page_path = config_dir().join(format!("{namespace}.yaml"));
        if page_path.exists() {
            if !self.save_page(namespace) {
                // Revert on failure so UI doesn't show a change that wasn't persisted.
                if let Some(page) = self.page_mut(namespace) {
                    if let Some(field) = page.fields.iter_mut().find(|f| f.key == key) {
                        field.value = old_value;
                    }
                }
            }
        }
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
        let page_path = config_dir().join(format!("{namespace}.yaml"));
        if page_path.exists() {
            if !self.save_page(namespace) {
                // Revert on failure.
                if let Some(page) = self.page_mut(namespace) {
                    if let Some(field) = page.fields.iter_mut().find(|f| f.key == key) {
                        field.value = old_value;
                    }
                }
            }
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
    pub fn set_last_provider(&mut self, id: &str) {
        self.set_value("providers", "_last_provider", id.to_string());
    }

    /// Get all provider field keys (provider IDs).
    pub fn provider_ids(&self) -> Vec<String> {
        let Some(page) = self.page_ref("providers") else {
            return Vec::new();
        };
        page.fields
            .iter()
            .filter(|f| f.field_type == ConfigFieldType::Provider)
            .map(|f| f.key.clone())
            .collect()
    }
}

// ── Migration ───────────────────────────────────────────────────────────────

/// Migrate old `providers.yaml` (flat map format) to CustomConfigPage format.
fn migrate_providers_file() {
    let old_path = bone_dir().join("config/providers.yaml");
    let new_path = bone_dir().join("config/providers.yaml");
    // Check if old file exists and new page doesn't exist yet
    if !old_path.exists() {
        return;
    }
    // If the file already parses as a CustomConfigPage, no migration needed
    if load_yaml::<CustomConfigPage>(&old_path).is_some() {
        return;
    }
    // Parse as old ProvidersConfig format
    let old_config = load_yaml::<crate::config::ProvidersConfig>(&old_path);
    let old_config = match old_config {
        Some(c) => c,
        None => return,
    };

    let mut fields: Vec<ConfigField> = Vec::new();
    for (id, entry) in &old_config.providers {
        let label = entry.label.clone();
        let nested = serde_yaml::to_value(entry).unwrap_or(serde_yaml::Value::Null);
        fields.push(ConfigField {
            key: id.clone(),
            label: Some(label),
            field_type: ConfigFieldType::Provider,
            options: Vec::new(),
            default: None,
            value: Some(nested),
        });
    }
    fields.push(ConfigField {
        key: "_last_provider".to_string(),
        label: None,
        field_type: ConfigFieldType::String,
        options: Vec::new(),
        default: None,
        value: Some(serde_yaml::Value::String(old_config.last_provider)),
    });

    let page = CustomConfigPage {
        title: "Providers".to_string(),
        fields,
    };
    if let Ok(yaml) = serde_yaml::to_string(&page) {
        let _ = std::fs::write(&new_path, yaml);
    }
}

/// Migrate the old `config-values.yaml` into individual page files, then remove it.
fn migrate_old_values_file() {
    use std::collections::BTreeMap;

    let values_path = bone_dir().join("config-values.yaml");
    if !values_path.exists() {
        return;
    }

    let Ok(raw) = std::fs::read_to_string(&values_path) else {
        return;
    };
    let raw = raw.trim_start_matches('\u{feff}');
    let Ok(values): Result<BTreeMap<String, BTreeMap<String, String>>, _> =
        serde_yaml::from_str(raw)
    else {
        return;
    };

    let dir = config_dir();
    for (namespace, kv) in &values {
        let page_path = dir.join(format!("{namespace}.yaml"));
        if !page_path.exists() {
            continue;
        }
        let Some(mut page) = load_yaml::<CustomConfigPage>(&page_path) else {
            continue;
        };
        for field in &mut page.fields {
            if let Some(val) = kv.get(&field.key) {
                field.value = Some(serde_yaml::Value::String(val.clone()));
            }
        }
        if let Ok(yaml) = serde_yaml::to_string(&page) {
            let _ = std::fs::write(&page_path, yaml);
        }
    }

    if let Some(kv) = values.get("general") {
        let page_path = dir.join("status.yaml");
        if page_path.exists()
            && let Some(mut page) = load_yaml::<CustomConfigPage>(&page_path)
        {
            let mut changed = false;
            for field in &mut page.fields {
                if is_status_toggle_key(&field.key)
                    && field.value.is_none()
                    && let Some(val) = kv.get(&field.key)
                {
                    field.value = Some(value_for_field(field, val.clone()));
                    changed = true;
                }
            }
            if changed && let Ok(yaml) = serde_yaml::to_string(&page) {
                let _ = std::fs::write(&page_path, yaml);
            }
        }
    }

    let _ = std::fs::remove_file(&values_path);
}

/// Move status toggles that were previously stored on `general.yaml` into
/// `status.yaml`. This covers users who already migrated from config-values.
fn migrate_status_values_from_general() {
    let dir = config_dir();
    let general_path = dir.join("general.yaml");
    let status_path = dir.join("status.yaml");
    if !general_path.exists() || !status_path.exists() {
        return;
    }

    let Some(general) = load_yaml::<CustomConfigPage>(&general_path) else {
        return;
    };
    let Some(mut status) = load_yaml::<CustomConfigPage>(&status_path) else {
        return;
    };

    let mut changed = false;
    for status_field in &mut status.fields {
        if !is_status_toggle_key(&status_field.key) || status_field.value.is_some() {
            continue;
        }
        let Some(general_field) = general
            .fields
            .iter()
            .find(|field| field.key == status_field.key)
        else {
            continue;
        };
        let Some(value) = general_field.value.clone() else {
            continue;
        };
        status_field.value = Some(value);
        changed = true;
    }

    if changed && let Ok(yaml) = serde_yaml::to_string(&status) {
        let _ = std::fs::write(status_path, yaml);
    }
}

fn is_status_toggle_key(key: &str) -> bool {
    UserConfig::STATUS_TOGGLE_KEYS.contains(&key)
}

fn value_for_field(field: &ConfigField, value: String) -> serde_yaml::Value {
    match field.field_type {
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
    }
}

#[cfg(test)]
#[path = "custom_tests.rs"]
mod custom_tests;

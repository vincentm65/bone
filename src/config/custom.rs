//! Custom user-defined config pages loaded from `~/.bone-rust/config/*.yaml`.
//!
//! Each page file (e.g. `general.yaml`, `tools.yaml`) contains both the field
//! schema *and* the current values. No separate values file is needed.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{bone_dir, load_yaml, seed_file_if_missing};

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
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ConfigFieldType {
    #[default]
    String,
    Number,
    Bool,
    Enum,
}

/// A parsed custom config page (from one YAML file).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CustomConfigPage {
    pub title: String,
    pub fields: Vec<ConfigField>,
}

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
const SUBAGENT_YAML: &str = include_str!("pages/subagent.yaml");
const TOOLS_YAML: &str = include_str!("pages/tools.yaml");
const SKILLS_YAML: &str = include_str!("pages/skills.yaml");

/// Seed built-in config pages into `~/.bone-rust/config/` if missing.
pub fn seed_builtin_pages() {
    let dir = config_dir();
    let _ = std::fs::create_dir_all(&dir);
    seed_file_if_missing(&dir.join("general.yaml"), GENERAL_YAML);
    seed_file_if_missing(&dir.join("subagent.yaml"), SUBAGENT_YAML);
    seed_file_if_missing(&dir.join("tools.yaml"), TOOLS_YAML);
    seed_file_if_missing(&dir.join("skills.yaml"), SKILLS_YAML);
}

// ── Load / save ─────────────────────────────────────────────────────────────

impl CustomConfigs {
    /// Scan `~/.bone-rust/config/` for `*.yaml` files and load them.
    pub fn load() -> Self {
        migrate_old_values_file();

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
            match load_yaml::<CustomConfigPage>(&path) {
                Some(page) => {
                    configs.pages.push((stem, page));
                }
                None => {
                    eprintln!("bone: warning: failed to parse {}", path.display());
                }
            }
        }

        configs.pages.sort_by(|a, b| a.0.cmp(&b.0));
        configs
    }

    /// Save a single page back to its YAML file.
    fn save_page(&self, namespace: &str) {
        let Some((_, page)) = self.pages.iter().find(|(ns, _)| ns == namespace) else {
            return;
        };
        let path = config_dir().join(format!("{namespace}.yaml"));
        if let Ok(yaml) = serde_yaml::to_string(page) {
            let _ = std::fs::write(path, yaml);
        }
    }

    /// Ensure the "tools" page has a bool field for every tool in the registry.
    /// New tools are added as enabled (true). Returns true if fields were added.
    pub fn sync_tools_from_registry(&mut self, tool_names: &[String]) -> bool {
        let pos = match self.pages.iter().position(|(ns, _)| ns == "tools") {
            Some(p) => p,
            None => return false,
        };
        let existing: std::collections::HashSet<&str> = self.pages[pos]
            .1
            .fields
            .iter()
            .map(|f| f.key.as_str())
            .collect();
        let new_names: Vec<&String> = tool_names
            .iter()
            .filter(|n| !existing.contains(n.as_str()))
            .collect();
        if new_names.is_empty() {
            return false;
        }
        let page = &mut self.pages[pos].1;
        for name in new_names {
            page.fields.push(ConfigField {
                key: name.clone(),
                label: Some(name.clone()),
                field_type: ConfigFieldType::Bool,
                options: Vec::new(),
                default: Some(serde_yaml::Value::Bool(true)),
                value: None,
            });
        }
        self.save_page("tools");
        true
    }

    /// Get all enabled tool names from the "tools" page.
    pub fn enabled_tool_names(&self) -> Vec<String> {
        let pos = match self.pages.iter().position(|(ns, _)| ns == "tools") {
            Some(p) => p,
            None => return Vec::new(),
        };
        let page = &self.pages[pos].1;
        page.fields
            .iter()
            .filter(|f| {
                let val = self.get_value("tools", &f.key);
                val == "true" || val.is_empty()
            })
            .map(|f| f.key.clone())
            .collect()
    }

    /// Sync skills from a list of skill names into the "skills" page.
    /// New skills are added as enabled (true). Returns true if fields were added.
    pub fn sync_skills_from_registry(&mut self, skill_names: &[String]) -> bool {
        let pos = match self.pages.iter().position(|(ns, _)| ns == "skills") {
            Some(p) => p,
            None => return false,
        };
        let existing: std::collections::HashSet<&str> = self.pages[pos]
            .1
            .fields
            .iter()
            .map(|f| f.key.as_str())
            .collect();
        let new_names: Vec<&String> = skill_names
            .iter()
            .filter(|n| !existing.contains(n.as_str()))
            .collect();
        if new_names.is_empty() {
            return false;
        }
        let page = &mut self.pages[pos].1;
        for name in new_names {
            page.fields.push(ConfigField {
                key: name.clone(),
                label: Some(name.clone()),
                field_type: ConfigFieldType::Bool,
                options: Vec::new(),
                default: Some(serde_yaml::Value::Bool(true)),
                value: None,
            });
        }
        self.save_page("skills");
        true
    }

    /// Get all enabled skill names from the "skills" page.
    pub fn enabled_skill_names(&self) -> Vec<String> {
        let pos = match self.pages.iter().position(|(ns, _)| ns == "skills") {
            Some(p) => p,
            None => return Vec::new(),
        };
        let page = &self.pages[pos].1;
        page.fields
            .iter()
            .filter(|f| {
                let val = self.get_value("skills", &f.key);
                val == "true" || val.is_empty()
            })
            .map(|f| f.key.clone())
            .collect()
    }

    /// Get the display value for a field, falling back to the default.
    pub fn get_value(&self, namespace: &str, key: &str) -> String {
        let page = self.pages.iter().find(|(ns, _)| ns == namespace);
        let Some((_, page)) = page else {
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
        let page = self.pages.iter_mut().find(|(ns, _)| ns == namespace);
        let Some((_, page)) = page else {
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
        field.value = Some(yaml_val);
        self.save_page(namespace);
    }

    /// Find a field definition by namespace and key.
    pub fn find_field(&self, namespace: &str, key: &str) -> Option<&ConfigField> {
        let page = self.pages.iter().find(|(ns, _)| ns == namespace)?;
        page.1.fields.iter().find(|f| f.key == key)
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
}

// ── Migration ───────────────────────────────────────────────────────────────

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

    let _ = std::fs::remove_file(&values_path);
}

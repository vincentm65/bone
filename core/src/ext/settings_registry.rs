//! Declarative settings schemas registered by Lua extensions.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

use crate::config::settings::{ExtensionValue, Settings, SettingsError};

pub type SharedSettingsRegistry = Arc<RwLock<SettingsRegistry>>;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SettingsRegistry {
    pages: BTreeMap<String, SettingsPage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsPage {
    pub namespace: String,
    pub title: String,
    #[serde(default)]
    pub owner: String,
    pub fields: Vec<SettingsField>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsField {
    pub key: String,
    pub label: String,
    #[serde(rename = "type")]
    pub field_type: SettingsFieldType,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    pub default: ExtensionValue,
    /// Resolved value, populated only in frontend/page snapshots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<ExtensionValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integer: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SettingsFieldType {
    String,
    Number,
    Bool,
    Enum,
}

impl SettingsRegistry {
    pub fn pages(&self) -> Vec<SettingsPage> {
        self.pages.values().cloned().collect()
    }

    pub fn register(&mut self, mut page: SettingsPage) -> Result<(), String> {
        validate_name("namespace", &page.namespace)?;
        if matches!(
            page.namespace.as_str(),
            "general" | "ui" | "theme" | "keymaps"
        ) {
            return Err(format!(
                "settings namespace '{}' is reserved",
                page.namespace
            ));
        }
        if let Some(existing) = self.pages.get(&page.namespace) {
            return Err(format!(
                "settings namespace '{}' is already registered by {}",
                page.namespace, existing.owner
            ));
        }
        if page.fields.is_empty() {
            return Err("settings page must contain at least one field".into());
        }
        let mut keys = BTreeSet::new();
        for field in &mut page.fields {
            field.value = None;
            validate_name("field key", &field.key)?;
            if !keys.insert(&field.key) {
                return Err(format!(
                    "duplicate settings key '{}.{}'",
                    page.namespace, field.key
                ));
            }
            field.validate(&field.default)?;
        }
        self.pages.insert(page.namespace.clone(), page);
        Ok(())
    }

    pub fn remove_owner(&mut self, owner: &str) {
        self.pages.retain(|_, page| page.owner != owner);
    }

    pub fn field(&self, path: &str) -> Option<(&SettingsPage, &SettingsField)> {
        let (namespace, key) = split_path(path).ok()?;
        let page = self.pages.get(namespace)?;
        Some((page, page.fields.iter().find(|field| field.key == key)?))
    }

    pub fn resolve(&self, settings: &Settings, path: &str) -> Result<ExtensionValue, String> {
        let (_, field) = self
            .field(path)
            .ok_or_else(|| format!("unknown extension setting: {path}"))?;
        if let Some(value) = settings.extension_value(path) {
            if field.validate(value).is_ok() {
                return Ok(value.clone());
            }
            crate::ext::ctx::runtime_warn_once(format!(
                "bone: warning: persisted value for {path} is invalid; using its registered default"
            ));
        }
        Ok(field.default.clone())
    }

    pub fn set(
        &self,
        settings: &mut Settings,
        path: &str,
        value: ExtensionValue,
        settings_path: &std::path::Path,
    ) -> Result<(), SettingsError> {
        let (_, field) = self.field(path).ok_or_else(|| {
            SettingsError::Validation(format!("unknown extension setting: {path}"))
        })?;
        field.validate(&value).map_err(SettingsError::Validation)?;
        settings.set_extension_value_at(path, value, settings_path)
    }
}

impl SettingsField {
    fn validate(&self, value: &ExtensionValue) -> Result<(), String> {
        match self.field_type {
            SettingsFieldType::String if !matches!(value, ExtensionValue::String(_)) => {
                return Err(format!("{} must be a string", self.key));
            }
            SettingsFieldType::Bool if !matches!(value, ExtensionValue::Bool(_)) => {
                return Err(format!("{} must be a boolean", self.key));
            }
            SettingsFieldType::Number => {
                let number = match value {
                    ExtensionValue::Number(value) if value.is_finite() => *value,
                    _ => return Err(format!("{} must be a finite number", self.key)),
                };
                if self.integer.unwrap_or(false) && number.fract() != 0.0 {
                    return Err(format!("{} must be an integer", self.key));
                }
                if self.min.is_some_and(|min| number < min) {
                    return Err(format!(
                        "{} must be at least {}",
                        self.key,
                        self.min.unwrap()
                    ));
                }
                if self.max.is_some_and(|max| number > max) {
                    return Err(format!(
                        "{} must be at most {}",
                        self.key,
                        self.max.unwrap()
                    ));
                }
            }
            SettingsFieldType::Enum => {
                let ExtensionValue::String(value) = value else {
                    return Err(format!("{} must be a string", self.key));
                };
                if self.options.is_empty() || !self.options.contains(value) {
                    return Err(format!(
                        "{} must be one of: {}",
                        self.key,
                        self.options.join(", ")
                    ));
                }
            }
            _ => {}
        }
        Ok(())
    }
}

pub fn split_path(path: &str) -> Result<(&str, &str), String> {
    let Some((namespace, key)) = path.split_once('.') else {
        return Err("setting path must be namespace.key".into());
    };
    if key.contains('.') {
        return Err("setting path must contain exactly one dot".into());
    }
    validate_name("namespace", namespace)?;
    validate_name("field key", key)?;
    Ok((namespace, key))
}

fn validate_name(kind: &str, value: &str) -> Result<(), String> {
    if value.is_empty()
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    {
        return Err(format!("invalid {kind}: {value:?}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(key: &str, field_type: SettingsFieldType, default: ExtensionValue) -> SettingsField {
        SettingsField {
            key: key.into(),
            label: key.into(),
            field_type,
            options: Vec::new(),
            default,
            value: None,
            integer: None,
            min: None,
            max: None,
        }
    }

    fn page(namespace: &str, fields: Vec<SettingsField>) -> SettingsPage {
        SettingsPage {
            namespace: namespace.into(),
            title: namespace.into(),
            owner: "test.lua".into(),
            fields,
        }
    }

    #[test]
    fn paths_and_names_are_strict() {
        assert_eq!(split_path("compact.auto").unwrap(), ("compact", "auto"));
        for path in ["compact", ".auto", "compact.", "a.b.c", "bad/name.key"] {
            assert!(split_path(path).is_err(), "accepted {path:?}");
        }
    }

    #[test]
    fn registration_rejects_collisions_and_bad_schemas() {
        let mut registry = SettingsRegistry::default();
        registry
            .register(page(
                "compact",
                vec![field(
                    "auto",
                    SettingsFieldType::Bool,
                    ExtensionValue::Bool(true),
                )],
            ))
            .unwrap();
        assert!(
            registry
                .register(page(
                    "compact",
                    vec![field(
                        "other",
                        SettingsFieldType::String,
                        ExtensionValue::String(String::new()),
                    )],
                ))
                .is_err()
        );
        assert!(
            registry
                .register(page(
                    "general",
                    vec![field(
                        "other",
                        SettingsFieldType::String,
                        ExtensionValue::String(String::new()),
                    )],
                ))
                .is_err()
        );
        assert!(registry.register(page("empty", Vec::new())).is_err());
        assert!(
            registry
                .register(page(
                    "bad",
                    vec![
                        field("same", SettingsFieldType::Bool, ExtensionValue::Bool(true)),
                        field("same", SettingsFieldType::Bool, ExtensionValue::Bool(false)),
                    ],
                ))
                .is_err()
        );
    }

    #[test]
    fn number_and_enum_constraints_are_enforced() {
        let mut number = field(
            "limit",
            SettingsFieldType::Number,
            ExtensionValue::Number(10.0),
        );
        number.integer = Some(true);
        number.min = Some(1.0);
        number.max = Some(100.0);
        assert!(number.validate(&ExtensionValue::Number(1.0)).is_ok());
        assert!(number.validate(&ExtensionValue::Number(1.5)).is_err());
        assert!(number.validate(&ExtensionValue::Number(101.0)).is_err());

        let mut choice = field(
            "mode",
            SettingsFieldType::Enum,
            ExtensionValue::String("fast".into()),
        );
        choice.options = vec!["fast".into(), "safe".into()];
        assert!(
            choice
                .validate(&ExtensionValue::String("safe".into()))
                .is_ok()
        );
        assert!(
            choice
                .validate(&ExtensionValue::String("other".into()))
                .is_err()
        );
    }
}

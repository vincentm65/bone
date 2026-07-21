//! Canonical peer configuration documents outside `config.yaml`.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::bone_dir;
use super::settings::{ExtensionValue, SubagentSettings};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SubagentsConfig {
    #[serde(default = "version_one")]
    pub version: u8,
    #[serde(default)]
    pub subagents: BTreeMap<String, SubagentSettings>,
}

impl Default for SubagentsConfig {
    fn default() -> Self {
        Self {
            version: 1,
            subagents: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionsConfig {
    #[serde(default = "version_one")]
    pub version: u8,
    #[serde(default)]
    pub extensions: BTreeMap<String, BTreeMap<String, ExtensionValue>>,
}

impl Default for ExtensionsConfig {
    fn default() -> Self {
        Self {
            version: 1,
            extensions: BTreeMap::new(),
        }
    }
}

fn version_one() -> u8 {
    1
}

pub fn subagents_path() -> PathBuf {
    bone_dir().join("subagents.yaml")
}

pub fn extensions_path() -> PathBuf {
    bone_dir().join("extensions.yaml")
}

pub fn load_providers() -> Result<Option<super::ProvidersConfig>, String> {
    let path = super::providers_path();
    let loaded: Option<super::ProvidersConfig> = load_versioned(&path)?;
    if let Some(config) = &loaded {
        validate_providers(config)?;
    }
    Ok(loaded)
}

pub(crate) fn validate_providers(config: &super::ProvidersConfig) -> Result<(), String> {
    if config.version != 1 {
        return Err(format!(
            "unsupported version {} in {}; expected 1",
            config.version,
            super::providers_path().display()
        ));
    }
    if !config.last_provider.is_empty() && !config.providers.contains_key(&config.last_provider) {
        return Err(format!(
            "active provider {:?} is not defined in {}",
            config.last_provider,
            super::providers_path().display()
        ));
    }
    Ok(())
}

pub fn load_subagents() -> Result<Option<SubagentsConfig>, String> {
    let path = subagents_path();
    let loaded: Option<SubagentsConfig> = load_versioned(&path)?;
    if let Some(config) = &loaded {
        validate_version(config.version, &path)?;
        super::settings::validate_subagents(&config.subagents)
            .map_err(|error| error.to_string())?;
    }
    Ok(loaded)
}

pub fn load_extensions() -> Result<Option<ExtensionsConfig>, String> {
    let path = extensions_path();
    let loaded: Option<ExtensionsConfig> = load_versioned(&path)?;
    if let Some(config) = &loaded {
        validate_version(config.version, &path)?;
    }
    Ok(loaded)
}

fn validate_version(version: u8, path: &Path) -> Result<(), String> {
    if version == 1 {
        Ok(())
    } else {
        Err(format!(
            "unsupported version {version} in {}; expected 1",
            path.display()
        ))
    }
}

fn load_versioned<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>, String> {
    if !path.exists() {
        return Ok(None);
    }
    super::load_yaml(path).map(Some)
}

pub fn persist_providers(values: &super::ProvidersConfig) -> Result<(), String> {
    write_document(&super::providers_path(), values, None)
}

pub fn persist_subagents(values: &BTreeMap<String, SubagentSettings>) -> Result<(), String> {
    super::settings::validate_subagents(values).map_err(|error| error.to_string())?;
    write_document(
        &subagents_path(),
        &SubagentsConfig {
            version: 1,
            subagents: values.clone(),
        },
        None,
    )
}

pub fn persist_extensions(
    values: &BTreeMap<String, BTreeMap<String, ExtensionValue>>,
) -> Result<(), String> {
    write_document(
        &extensions_path(),
        &ExtensionsConfig {
            version: 1,
            extensions: values.clone(),
        },
        None,
    )
}

pub(crate) fn write_document<T: Serialize>(
    path: &Path,
    value: &T,
    permissions_from: Option<&Path>,
) -> Result<(), String> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let yaml = serde_yaml::to_string(value).map_err(|error| error.to_string())?;
    let permissions = std::fs::metadata(path)
        .or_else(|_| {
            permissions_from
                .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))
                .and_then(std::fs::metadata)
        })
        .ok()
        .map(|metadata| metadata.permissions());
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    temporary
        .write_all(yaml.as_bytes())
        .map_err(|error| error.to_string())?;
    if let Some(permissions) = permissions {
        temporary
            .as_file()
            .set_permissions(permissions)
            .map_err(|error| error.to_string())?;
    }
    temporary
        .as_file_mut()
        .sync_all()
        .map_err(|error| error.to_string())?;
    temporary
        .persist(path)
        .map_err(|error| error.error.to_string())?;
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| error.to_string())
}

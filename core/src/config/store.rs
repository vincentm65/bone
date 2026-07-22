//! Daemon-owned aggregate configuration service.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use bone_protocol::{
    ConfigPage, ConfigSchema, ConfigSnapshot, ProviderConfig, ProviderUpdate, SettingDefinition,
};

use super::settings::{ExtensionValue, Settings, SubagentSettings};

#[derive(Clone)]
pub struct ConfigStore {
    inner: Arc<Mutex<Inner>>,
    extensions: Arc<Mutex<Vec<crate::ext::ExtensionManager>>>,
}

/// Typed aggregate owned by the daemon configuration service.
struct Inner {
    revision: u64,
    core: Settings,
    providers: super::ProvidersConfig,
    subagents: BTreeMap<String, SubagentSettings>,
    extension_values: BTreeMap<String, BTreeMap<String, ExtensionValue>>,
    disabled_tools: Vec<String>,
    disabled_commands: Vec<String>,
    legacy: super::custom::CustomConfigs,
}

impl ConfigStore {
    pub fn new(extensions: crate::ext::ExtensionManager) -> Result<Self, String> {
        super::migration::migrate()?;
        let path = super::settings::settings_path();
        let core = super::settings::Settings::load()
            .map_err(|error| format!("cannot load {}: {error}", path.display()))?
            .ok_or_else(|| format!("configuration migration did not create {}", path.display()))?;
        Ok(Self::from_legacy(
            extensions,
            super::custom::CustomConfigs {
                settings: Some(core),
                ..Default::default()
            },
        ))
    }

    pub(crate) fn from_legacy(
        extensions: crate::ext::ExtensionManager,
        legacy: super::custom::CustomConfigs,
    ) -> Self {
        let core = legacy.settings.clone().unwrap_or_else(Settings::defaults);
        let legacy_version = core.resolved().version;
        let providers = match super::domains::load_providers() {
            Ok(Some(config)) => config,
            Ok(None) => legacy.derive_providers_config(),
            Err(error) => {
                crate::ext::ctx::runtime_warn_once(format!("bone: warning: {error}"));
                super::ProvidersConfig::default()
            }
        };
        let subagents = match super::domains::load_subagents() {
            Ok(Some(config)) => config.subagents,
            Ok(None) => core.resolved().subagents.clone(),
            Err(error) => {
                crate::ext::ctx::runtime_warn_once(format!("bone: warning: {error}"));
                BTreeMap::new()
            }
        };
        let extension_values = match super::domains::load_extensions() {
            Ok(Some(config)) => config.extensions,
            Ok(None) => core.resolved().extensions.clone(),
            Err(error) => {
                crate::ext::ctx::runtime_warn_once(format!("bone: warning: {error}"));
                BTreeMap::new()
            }
        };
        let disabled_tools = if legacy_version >= 2 {
            core.resolved().tools.disabled.clone()
        } else {
            legacy.disabled_names("tools")
        };
        let disabled_commands = if legacy_version >= 2 {
            core.resolved().commands.disabled.clone()
        } else {
            legacy.disabled_names("commands")
        };
        let mut runtime_settings = core.clone();
        runtime_settings.replace_domains(subagents.clone(), extension_values.clone());
        extensions.replace_settings(runtime_settings);
        let revision = core.revision();
        Self {
            inner: Arc::new(Mutex::new(Inner {
                revision,
                core,
                providers,
                subagents,
                extension_values,
                disabled_tools,
                disabled_commands,
                legacy,
            })),
            extensions: Arc::new(Mutex::new(vec![extensions])),
        }
    }

    /// Attach an extension runtime and keep it synchronized with future mutations.
    pub fn attach_extensions(&self, extensions: crate::ext::ExtensionManager) {
        self.sync_extension(&extensions);
        let mut managers = self
            .extensions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !managers
            .iter()
            .any(|manager| manager.same_runtime(&extensions))
        {
            managers.push(extensions);
        }
    }

    /// Replace one actor's runtime after an extension reload.
    pub fn replace_extensions(
        &self,
        old: &crate::ext::ExtensionManager,
        new: crate::ext::ExtensionManager,
    ) {
        self.sync_extension(&new);
        let mut managers = self
            .extensions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        managers.retain(|manager| !manager.same_runtime(old));
        if !managers.iter().any(|manager| manager.same_runtime(&new)) {
            managers.push(new);
        }
    }

    fn sync_extension(&self, extensions: &crate::ext::ExtensionManager) {
        let settings = {
            let inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
            Self::runtime_settings(&inner)
        };
        extensions.replace_settings(settings);
        extensions.replace_config_snapshot(self.legacy_snapshot());
    }

    fn sync_extensions(&self, settings: Settings, legacy: super::custom::CustomConfigs) {
        for extensions in self.extension_managers() {
            extensions.replace_settings(settings.clone());
            extensions.replace_config_snapshot(legacy.clone());
        }
    }

    fn runtime_settings(inner: &Inner) -> Settings {
        let mut settings = inner.core.clone();
        settings.replace_domains(inner.subagents.clone(), inner.extension_values.clone());
        settings
    }

    fn extension_manager(&self) -> crate::ext::ExtensionManager {
        self.extensions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .last()
            .expect("config store always has an extension manager")
            .clone()
    }

    fn extension_managers(&self) -> Vec<crate::ext::ExtensionManager> {
        self.extensions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub fn providers_config(&self) -> super::ProvidersConfig {
        self.inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .providers
            .clone()
    }

    /// Compatibility snapshot for runtime surfaces not yet migrated from the
    /// legacy page model. The daemon performs the only filesystem load.
    pub fn legacy_snapshot(&self) -> super::custom::CustomConfigs {
        let inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        let mut legacy = inner.legacy.clone();
        legacy.settings = Some(Self::runtime_settings(&inner));
        legacy
    }

    pub fn provider_candidate_config(
        &self,
        update: &ProviderUpdate,
        expected: u64,
    ) -> Result<super::ProvidersConfig, (u64, String)> {
        let inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        if expected != inner.revision {
            return Err((
                inner.revision,
                format!(
                    "configuration changed; expected revision {expected}, current revision {}",
                    inner.revision
                ),
            ));
        }
        super::providers_config::validate_reasoning_effort(&update.reasoning_effort)
            .map_err(|error| (inner.revision, error))?;
        let mut config = inner.providers.clone();
        let api_key = update.api_key.clone().map(Into::into).unwrap_or_else(|| {
            config
                .providers
                .get(&update.id)
                .map(|entry| entry.api_key.clone())
                .unwrap_or_default()
        });
        config.providers.insert(
            update.id.clone(),
            super::ProviderEntry {
                label: update.label.clone(),
                base_url: update.base_url.clone(),
                model: update.model.clone(),
                api_key,
                endpoint: update.endpoint.clone(),
                handler: update.handler.clone(),
                context_window_tokens: update.context_window_tokens,
                reasoning_effort: update.reasoning_effort.clone(),
            },
        );
        Ok(config)
    }

    pub fn check_revision(&self, expected: u64) -> Result<(), (u64, String)> {
        let inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        if expected == inner.revision {
            Ok(())
        } else {
            Err((
                inner.revision,
                format!(
                    "configuration changed; expected revision {expected}, current revision {}",
                    inner.revision
                ),
            ))
        }
    }

    pub fn reload_settings(&self) -> Result<(), String> {
        let path = super::settings::settings_path();
        let loaded = super::settings::Settings::load()
            .map_err(|error| super::error::ConfigError::load(&path, error.to_string()).to_string())?
            .ok_or_else(|| {
                super::error::ConfigError::load(&path, "file does not exist").to_string()
            })?;
        let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        inner.disabled_tools = loaded.resolved().tools.disabled.clone();
        inner.disabled_commands = loaded.resolved().commands.disabled.clone();
        inner.core = loaded;
        inner.revision = inner.revision.saturating_add(1);
        let settings = Self::runtime_settings(&inner);
        let mut legacy = inner.legacy.clone();
        legacy.settings = Some(settings.clone());
        drop(inner);
        self.sync_extensions(settings, legacy);
        Ok(())
    }

    pub fn schema(&self) -> ConfigSchema {
        self.schema_for(&[], &[])
    }

    pub fn schema_for(&self, tool_names: &[String], command_names: &[String]) -> ConfigSchema {
        fn field(
            path: &str,
            key: &str,
            label: &str,
            value_type: &str,
            options: &[&str],
            default: serde_json::Value,
        ) -> SettingDefinition {
            SettingDefinition {
                path: path.into(),
                key: key.into(),
                label: label.into(),
                value_type: value_type.into(),
                options: options.iter().map(|value| (*value).into()).collect(),
                default,
                value: None,
                integer: None,
                min: None,
                max: None,
                reload_behavior: "immediate".into(),
            }
        }

        let extension_pages = self
            .extension_manager()
            .extension_settings_pages()
            .into_iter()
            .map(|page| ConfigPage {
                namespace: page.namespace.clone(),
                title: page.title,
                fields: page
                    .fields
                    .into_iter()
                    .map(|extension| SettingDefinition {
                        path: format!("extensions.{}.{}", page.namespace, extension.key),
                        key: extension.key,
                        label: extension.label,
                        value_type: match extension.field_type {
                            crate::ext::settings_registry::SettingsFieldType::String => "string",
                            crate::ext::settings_registry::SettingsFieldType::Number => "number",
                            crate::ext::settings_registry::SettingsFieldType::Bool => "bool",
                            crate::ext::settings_registry::SettingsFieldType::Enum => "enum",
                        }
                        .into(),
                        options: extension.options,
                        default: serde_json::to_value(extension.default).unwrap_or_default(),
                        value: extension
                            .value
                            .and_then(|value| serde_json::to_value(value).ok()),
                        integer: extension.integer,
                        min: extension.min,
                        max: extension.max,
                        reload_behavior: "immediate".into(),
                    })
                    .collect(),
                pages: Vec::new(),
            })
            .collect();
        let mut tool_names = tool_names.to_vec();
        tool_names.sort();
        tool_names.dedup();
        let tool_fields = tool_names
            .into_iter()
            .map(|name| {
                field(
                    &format!("tools.{name}"),
                    &name,
                    &name,
                    "bool",
                    &[],
                    serde_json::json!(true),
                )
            })
            .collect();
        let mut command_names: Vec<_> = command_names
            .iter()
            .filter(|name| !crate::commands::is_protected_builtin(name))
            .cloned()
            .collect();
        command_names.sort();
        command_names.dedup();
        let command_fields = command_names
            .into_iter()
            .map(|name| {
                field(
                    &format!("commands.{name}"),
                    &name,
                    &name,
                    "bool",
                    &[],
                    serde_json::json!(true),
                )
            })
            .collect();
        ConfigSchema {
            pages: vec![
                ConfigPage {
                    namespace: "general".into(),
                    title: "General".into(),
                    fields: vec![
                        field(
                            "general.approval",
                            "approval",
                            "Approval mode",
                            "enum",
                            &["safe", "danger"],
                            serde_json::json!("safe"),
                        ),
                        field(
                            "general.show_reasoning",
                            "show_reasoning",
                            "Show reasoning",
                            "bool",
                            &[],
                            serde_json::json!(false),
                        ),
                        field(
                            "ui.input.preset",
                            "input_preset",
                            "Input style",
                            "enum",
                            &["custom", "lines", "box", "filled"],
                            serde_json::Value::Null,
                        ),
                    ],
                    pages: Vec::new(),
                },
                ConfigPage {
                    namespace: "providers".into(),
                    title: "Providers".into(),
                    fields: Vec::new(),
                    pages: Vec::new(),
                },
                ConfigPage {
                    namespace: "tools".into(),
                    title: "Tools".into(),
                    fields: tool_fields,
                    pages: Vec::new(),
                },
                ConfigPage {
                    namespace: "commands".into(),
                    title: "Commands".into(),
                    fields: command_fields,
                    pages: Vec::new(),
                },
                ConfigPage {
                    namespace: "status".into(),
                    title: "Status".into(),
                    fields: vec![
                        field(
                            "ui.status_show_model",
                            "show_model",
                            "Model",
                            "bool",
                            &[],
                            serde_json::json!(true),
                        ),
                        field(
                            "ui.status_show_approval",
                            "show_approval",
                            "Approval",
                            "bool",
                            &[],
                            serde_json::json!(true),
                        ),
                        field(
                            "ui.status_show_tokens_curr",
                            "show_tokens_curr",
                            "Current tokens",
                            "bool",
                            &[],
                            serde_json::json!(true),
                        ),
                        field(
                            "ui.status_show_tokens_in",
                            "show_tokens_in",
                            "Input tokens",
                            "bool",
                            &[],
                            serde_json::json!(true),
                        ),
                        field(
                            "ui.status_show_tokens_out",
                            "show_tokens_out",
                            "Output tokens",
                            "bool",
                            &[],
                            serde_json::json!(true),
                        ),
                        field(
                            "ui.status_show_tokens_total",
                            "show_tokens_total",
                            "Total tokens",
                            "bool",
                            &[],
                            serde_json::json!(true),
                        ),
                        field(
                            "ui.status_show_queue",
                            "show_queue",
                            "Queue length",
                            "bool",
                            &[],
                            serde_json::json!(true),
                        ),
                        field(
                            "ui.status_show_spinner",
                            "show_spinner",
                            "Spinner",
                            "bool",
                            &[],
                            serde_json::json!(true),
                        ),
                        field(
                            "ui.spinner_text_rotate",
                            "spinner_text_rotate",
                            "Rotate spinner text",
                            "bool",
                            &[],
                            serde_json::json!(true),
                        ),
                        field(
                            "ui.status_show_timer",
                            "show_timer",
                            "Timer",
                            "bool",
                            &[],
                            serde_json::json!(true),
                        ),
                        field(
                            "ui.spinner_style",
                            "spinner_style",
                            "Spinner style",
                            "enum",
                            &[
                                "braille",
                                "triangle",
                                "pipe",
                                "kaomoji",
                                "typing",
                                "waveline",
                                "dots_text",
                                "progblock",
                            ],
                            serde_json::json!("braille"),
                        ),
                        field(
                            "ui.spinner_text",
                            "spinner_text",
                            "Spinner text preset",
                            "enum",
                            &["thinking", "pondering", "processing"],
                            serde_json::json!("thinking"),
                        ),
                        field(
                            "ui.spinner_custom",
                            "spinner_custom",
                            "Custom spinner phrases",
                            "string",
                            &[],
                            serde_json::json!(""),
                        ),
                        field(
                            "ui.spinner_speed",
                            "spinner_speed",
                            "Spinner speed (ms)",
                            "number",
                            &[],
                            serde_json::json!(0),
                        ),
                        field(
                            "ui.spinner_text_speed",
                            "spinner_text_speed",
                            "Spinner text speed (ms)",
                            "number",
                            &[],
                            serde_json::json!(0),
                        ),
                    ],
                    pages: Vec::new(),
                },
                ConfigPage {
                    namespace: "extensions".into(),
                    title: "Extensions".into(),
                    fields: Vec::new(),
                    pages: extension_pages,
                },
            ],
        }
    }

    pub fn snapshot(&self) -> ConfigSnapshot {
        let inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        let mut entries: Vec<_> = inner
            .providers
            .providers
            .iter()
            .map(|(id, provider)| ProviderConfig {
                id: id.clone(),
                label: provider.label.clone(),
                base_url: provider.base_url.clone(),
                model: provider.model.clone(),
                endpoint: provider.endpoint.clone(),
                handler: provider.handler.clone(),
                context_window_tokens: provider.context_window_tokens,
                reasoning_effort: provider.reasoning_effort.clone(),
                api_key_configured: !provider.api_key.is_empty(),
            })
            .collect();
        entries.sort_by(|left, right| left.id.cmp(&right.id));
        let mut values = serde_json::to_value(inner.core.resolved()).unwrap_or_default();
        if let Some(values) = values.as_object_mut() {
            values.insert(
                "subagents".into(),
                serde_json::to_value(&inner.subagents).unwrap_or_default(),
            );
            values.insert(
                "extensions".into(),
                serde_json::to_value(&inner.extension_values).unwrap_or_default(),
            );
        }
        ConfigSnapshot {
            revision: inner.revision,
            values,
            providers: entries,
            active_provider: inner.providers.last_provider.clone(),
            disabled_tools: inner.disabled_tools.clone(),
            disabled_commands: inner.disabled_commands.clone(),
        }
    }

    fn mutate<T>(
        &self,
        expected: u64,
        mutation: impl FnOnce(&mut Inner) -> Result<T, String>,
    ) -> Result<T, (u64, String)> {
        let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        if expected != inner.revision {
            return Err((
                inner.revision,
                format!(
                    "configuration changed; expected revision {expected}, current revision {}",
                    inner.revision
                ),
            ));
        }
        match mutation(&mut inner) {
            Ok(value) => {
                inner.revision = inner.revision.saturating_add(1);
                let settings = Self::runtime_settings(&inner);
                let mut legacy = inner.legacy.clone();
                legacy.settings = Some(settings.clone());
                drop(inner);
                self.sync_extensions(settings, legacy);
                Ok(value)
            }
            Err(error) => Err((inner.revision, error)),
        }
    }

    pub fn set_value(
        &self,
        path: &str,
        value: serde_json::Value,
        expected: u64,
    ) -> Result<(), (u64, String)> {
        let file = super::settings::settings_path();
        if let Some(extension_path) = path.strip_prefix("extensions.") {
            let file = super::domains::extensions_path();
            return self.mutate(expected, |inner| {
                let value = serde_json::from_value(value).map_err(|error| {
                    super::error::ConfigError::persist(&file, error.to_string())
                        .at_setting(path)
                        .to_string()
                })?;
                self.extension_manager()
                    .validate_extension_setting(extension_path, &value)
                    .map_err(|error| {
                        super::error::ConfigError::persist(&file, error)
                            .at_setting(path)
                            .to_string()
                    })?;
                let (namespace, key) = extension_path
                    .split_once('.')
                    .ok_or_else(|| format!("invalid extension setting path: {path}"))?;
                let mut candidate = inner.extension_values.clone();
                candidate
                    .entry(namespace.to_string())
                    .or_default()
                    .insert(key.to_string(), value);
                super::domains::persist_extensions(&candidate).map_err(|error| {
                    super::error::ConfigError::persist(&file, error)
                        .at_setting(path)
                        .to_string()
                })?;
                inner.extension_values = candidate;
                Ok(())
            });
        }
        self.mutate(expected, |inner| {
            let mut candidate = inner.core.clone();
            candidate.set_path_at(path, value, &file).map_err(|error| {
                super::error::ConfigError::persist(&file, error.to_string())
                    .at_setting(path)
                    .to_string()
            })?;
            inner.core = candidate;
            Ok(())
        })
    }

    pub fn reset_value(&self, path: &str, expected: u64) -> Result<(), (u64, String)> {
        if let Some(extension_path) = path.strip_prefix("extensions.") {
            let file = super::domains::extensions_path();
            return self.mutate(expected, |inner| {
                let (namespace, key) = extension_path
                    .split_once('.')
                    .ok_or_else(|| format!("invalid extension setting path: {path}"))?;
                let mut candidate = inner.extension_values.clone();
                if let Some(values) = candidate.get_mut(namespace) {
                    values.remove(key);
                    if values.is_empty() {
                        candidate.remove(namespace);
                    }
                }
                super::domains::persist_extensions(&candidate).map_err(|error| {
                    super::error::ConfigError::persist(&file, error)
                        .at_setting(path)
                        .to_string()
                })?;
                inner.extension_values = candidate;
                Ok(())
            });
        }
        let default = super::settings::Settings::defaults()
            .get_path(path)
            .map_err(|error| (self.snapshot().revision, error.to_string()))?;
        self.set_value(path, default, expected)
    }

    pub fn upsert_provider(
        &self,
        update: ProviderUpdate,
        expected: u64,
    ) -> Result<(), (u64, String)> {
        self.mutate(expected, |inner| {
            super::providers_config::validate_reasoning_effort(&update.reasoning_effort)?;
            let api_key = update.api_key.map(Into::into).unwrap_or_else(|| {
                inner
                    .providers
                    .providers
                    .get(&update.id)
                    .map(|entry| entry.api_key.clone())
                    .unwrap_or_default()
            });
            let entry = super::ProviderEntry {
                label: update.label,
                base_url: update.base_url,
                model: update.model,
                api_key,
                endpoint: update.endpoint,
                handler: update.handler,
                context_window_tokens: update.context_window_tokens,
                reasoning_effort: update.reasoning_effort,
            };
            let mut candidate = inner.providers.clone();
            candidate.providers.insert(update.id.clone(), entry);
            super::domains::persist_providers(&candidate)?;
            inner.providers = candidate;
            Ok(())
        })
    }

    pub fn delete_provider(&self, id: &str, expected: u64) -> Result<(), (u64, String)> {
        self.mutate(expected, |inner| {
            if inner.providers.last_provider == id {
                return Err("cannot delete the active provider".into());
            }
            let mut candidate = inner.providers.clone();
            if candidate.providers.remove(id).is_none() {
                return Err(format!("unknown provider: {id}"));
            }
            super::domains::persist_providers(&candidate)?;
            inner.providers = candidate;
            Ok(())
        })
    }

    pub fn set_active_provider(&self, id: &str, expected: u64) -> Result<(), (u64, String)> {
        self.mutate(expected, |inner| {
            if !inner.providers.providers.contains_key(id) {
                return Err(format!("unknown provider: {id}"));
            }
            let mut candidate = inner.providers.clone();
            candidate.last_provider = id.to_string();
            super::domains::persist_providers(&candidate)?;
            inner.providers = candidate;
            Ok(())
        })
    }

    pub fn set_enabled(
        &self,
        namespace: &str,
        name: &str,
        enabled: bool,
        expected: u64,
    ) -> Result<(), (u64, String)> {
        let file = super::settings::settings_path();
        self.mutate(expected, |inner| {
            let mut disabled = match namespace {
                "tools" => inner.disabled_tools.clone(),
                "commands" => inner.disabled_commands.clone(),
                _ => return Err(format!("unknown enablement namespace: {namespace}")),
            };
            disabled.retain(|entry| entry != name);
            if !enabled {
                disabled.push(name.to_string());
                disabled.sort();
                disabled.dedup();
            }
            let mut candidate = inner.core.clone();
            candidate.inner.version = 2;
            match namespace {
                "tools" => candidate.inner.tools.disabled = disabled.clone(),
                "commands" => candidate.inner.commands.disabled = disabled.clone(),
                _ => unreachable!(),
            }
            candidate.save().map_err(|error| {
                super::error::ConfigError::persist(&file, error.to_string()).to_string()
            })?;
            inner.core = candidate;
            match namespace {
                "tools" => inner.disabled_tools = disabled,
                "commands" => inner.disabled_commands = disabled,
                _ => unreachable!(),
            }
            Ok(())
        })
    }

    fn mutate_subagents(
        &self,
        expected: u64,
        setting: &str,
        mutation: impl FnOnce(&mut BTreeMap<String, SubagentSettings>) -> Result<(), String>,
    ) -> Result<(), (u64, String)> {
        let file = super::domains::subagents_path();
        self.mutate(expected, |inner| {
            let mut candidate = inner.subagents.clone();
            mutation(&mut candidate).map_err(|error| {
                super::error::ConfigError::persist(&file, error)
                    .at_setting(setting)
                    .to_string()
            })?;
            super::domains::persist_subagents(&candidate).map_err(|error| {
                super::error::ConfigError::persist(&file, error)
                    .at_setting(setting)
                    .to_string()
            })?;
            inner.subagents = candidate;
            Ok(())
        })
    }

    pub fn upsert_subagent(
        &self,
        agent: bone_protocol::SubagentDefinition,
        expected: u64,
    ) -> Result<(), (u64, String)> {
        let setting = format!("subagents.{}", agent.name);
        let name = agent.name.clone();
        let value = SubagentSettings {
            description: agent.description,
            system_prompt: agent.system_prompt.filter(|value| !value.is_empty()),
            provider: agent.provider.filter(|value| !value.is_empty()),
            model: agent.model.filter(|value| !value.is_empty()),
            approval: agent.approval,
            timeout_ms: agent.timeout_ms,
            max_concurrency: agent.max_concurrency,
            enabled: agent.enabled,
        };
        self.mutate_subagents(expected, &setting, move |candidate| {
            candidate.insert(name, value);
            Ok(())
        })
    }

    pub fn delete_subagent(&self, name: &str, expected: u64) -> Result<(), (u64, String)> {
        let setting = format!("subagents.{name}");
        let name = name.to_string();
        self.mutate_subagents(expected, &setting, move |candidate| {
            candidate.remove(&name);
            Ok(())
        })
    }

    pub fn set_subagent_enabled(
        &self,
        name: &str,
        enabled: bool,
        expected: u64,
    ) -> Result<(), (u64, String)> {
        let setting = format!("subagents.{name}.enabled");
        let name = name.to_string();
        self.mutate_subagents(expected, &setting, move |candidate| {
            let agent = candidate
                .get_mut(&name)
                .ok_or_else(|| format!("unknown config sub-agent: {name}"))?;
            agent.enabled = enabled;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_mutation_rejects_unsupported_reasoning_effort() {
        let _guard = crate::util::test_env_lock();
        let previous = std::env::var_os("BONE_DIR");
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("BONE_DIR", dir.path()) };

        let store = ConfigStore::new(crate::ext::ExtensionManager::unloaded()).unwrap();
        let before = store.snapshot();
        let update = ProviderUpdate {
            id: "test".into(),
            label: "Test".into(),
            base_url: "http://localhost".into(),
            model: "test-model".into(),
            endpoint: "/chat/completions".into(),
            handler: "openai".into(),
            context_window_tokens: None,
            reasoning_effort: "extreme".into(),
            api_key: None,
        };

        let error = store.upsert_provider(update, before.revision).unwrap_err();
        assert!(error.1.contains("unsupported reasoning_effort"));
        let after = store.snapshot();
        assert_eq!(after.revision, before.revision);
        assert_eq!(after.providers, before.providers);

        unsafe {
            match previous {
                Some(value) => std::env::set_var("BONE_DIR", value),
                None => std::env::remove_var("BONE_DIR"),
            }
        }
    }

    #[test]
    fn malformed_startup_configuration_returns_error_instead_of_panicking() {
        let _guard = crate::util::test_env_lock();
        let previous = std::env::var_os("BONE_DIR");
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("BONE_DIR", dir.path()) };
        std::fs::write(dir.path().join("config.yaml"), "version: 2\ngeneral: [\n").unwrap();

        let error = ConfigStore::new(crate::ext::ExtensionManager::unloaded())
            .err()
            .expect("malformed configuration should fail");
        assert!(error.contains("cannot migrate"));

        unsafe {
            match previous {
                Some(value) => std::env::set_var("BONE_DIR", value),
                None => std::env::remove_var("BONE_DIR"),
            }
        }
    }

    #[test]
    fn successful_migration_makes_legacy_pages_inert() {
        let _guard = crate::util::test_env_lock();
        let old_bone = std::env::var_os("BONE_DIR");
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("BONE_DIR", dir.path()) };

        super::super::migration::migrate().unwrap();
        let legacy_dir = dir.path().join("config");
        std::fs::create_dir_all(&legacy_dir).unwrap();
        let legacy = "this is not a valid config page\n";
        std::fs::write(legacy_dir.join("general.yaml"), legacy).unwrap();

        let store = ConfigStore::new(crate::ext::ExtensionManager::unloaded()).unwrap();

        assert_eq!(store.snapshot().values["general"]["approval"], "safe");
        assert!(store.legacy_snapshot().pages.is_empty());
        assert_eq!(
            std::fs::read_to_string(legacy_dir.join("general.yaml")).unwrap(),
            legacy
        );
        unsafe {
            match old_bone {
                Some(value) => std::env::set_var("BONE_DIR", value),
                None => std::env::remove_var("BONE_DIR"),
            }
        }
    }

    #[test]
    fn failed_persistence_keeps_revision_and_typed_state() {
        let _guard = crate::util::test_env_lock();
        let old_bone = std::env::var_os("BONE_DIR");
        let dir = std::env::temp_dir().join(format!(
            "bone-failed-config-store-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        unsafe { std::env::set_var("BONE_DIR", &dir) };

        let store = ConfigStore::new(crate::ext::ExtensionManager::unloaded()).unwrap();
        let blocked = super::super::settings::settings_path();
        std::fs::remove_file(&blocked).unwrap();
        std::fs::create_dir(&blocked).unwrap();
        let before = store.snapshot();
        let error = store
            .set_enabled("tools", "shell", false, before.revision)
            .unwrap_err();

        let after = store.snapshot();
        assert_eq!(after.revision, before.revision);
        assert_eq!(after.disabled_tools, before.disabled_tools);
        assert!(error.1.contains(&blocked.display().to_string()));

        std::fs::remove_dir_all(dir).ok();
        unsafe {
            match old_bone {
                Some(value) => std::env::set_var("BONE_DIR", value),
                None => std::env::remove_var("BONE_DIR"),
            }
        }
    }

    #[test]
    fn clones_share_mutations_and_revision() {
        let _guard = crate::util::test_env_lock();
        let old_bone = std::env::var_os("BONE_DIR");
        let dir = std::env::temp_dir().join(format!(
            "bone-shared-config-store-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        unsafe { std::env::set_var("BONE_DIR", &dir) };

        let store = ConfigStore::new(crate::ext::ExtensionManager::unloaded()).unwrap();
        let other_actor_store = store.clone();
        let revision = store.snapshot().revision;
        other_actor_store
            .set_value("general.show_reasoning", true.into(), revision)
            .unwrap();

        let snapshot = store.snapshot();
        assert_eq!(snapshot.revision, revision + 1);
        assert_eq!(snapshot.values["general"]["show_reasoning"], true);

        std::fs::remove_dir_all(dir).ok();
        unsafe {
            match old_bone {
                Some(value) => std::env::set_var("BONE_DIR", value),
                None => std::env::remove_var("BONE_DIR"),
            }
        }
    }

    #[test]
    fn reload_settings_adopts_config_yaml_and_advances_revision() {
        let _guard = crate::util::test_env_lock();
        let old_bone = std::env::var_os("BONE_DIR");
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("BONE_DIR", dir.path()) };

        let extensions = crate::ext::ExtensionManager::unloaded();
        let store = ConfigStore::new(extensions.clone()).unwrap();
        let before = store.snapshot();
        let mut persisted = Settings::load().unwrap().unwrap();
        persisted
            .set_value("general", "show_thinking", "true".into())
            .unwrap();

        store.reload_settings().unwrap();

        let after = store.snapshot();
        assert_eq!(after.revision, before.revision + 1);
        assert_eq!(after.values["general"]["show_reasoning"], true);
        assert_eq!(
            extensions
                .config_snapshot()
                .get_value("general", "show_thinking"),
            "true"
        );

        unsafe {
            match old_bone {
                Some(value) => std::env::set_var("BONE_DIR", value),
                None => std::env::remove_var("BONE_DIR"),
            }
        }
    }

    #[test]
    fn reload_settings_does_not_adopt_peer_documents() {
        let _guard = crate::util::test_env_lock();
        let old_bone = std::env::var_os("BONE_DIR");
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("BONE_DIR", dir.path()) };

        let store = ConfigStore::new(crate::ext::ExtensionManager::unloaded()).unwrap();
        let before = store.snapshot();

        let mut subagents = super::super::domains::load_subagents()
            .unwrap()
            .unwrap_or_default()
            .subagents;
        subagents.insert(
            "external-reviewer".into(),
            SubagentSettings {
                description: "External edit".into(),
                ..Default::default()
            },
        );
        super::super::domains::persist_subagents(&subagents).unwrap();

        let mut extension_values = super::super::domains::load_extensions()
            .unwrap()
            .unwrap_or_default()
            .extensions;
        extension_values
            .entry("external".into())
            .or_default()
            .insert("enabled".into(), ExtensionValue::Bool(true));
        super::super::domains::persist_extensions(&extension_values).unwrap();

        let mut providers = super::super::domains::load_providers()
            .unwrap()
            .unwrap_or_default();
        providers.providers.insert(
            "external".into(),
            super::super::ProviderEntry {
                label: "External".into(),
                base_url: "http://localhost".into(),
                model: "external-model".into(),
                api_key: Default::default(),
                endpoint: "/chat/completions".into(),
                handler: "openai".into(),
                context_window_tokens: None,
                reasoning_effort: String::new(),
            },
        );
        super::super::domains::persist_providers(&providers).unwrap();

        store.reload_settings().unwrap();

        let after = store.snapshot();
        assert_eq!(after.revision, before.revision + 1);
        assert_eq!(after.values["subagents"], before.values["subagents"]);
        assert_eq!(after.values["extensions"], before.values["extensions"]);
        assert_eq!(after.providers, before.providers);
        assert!(
            super::super::domains::load_subagents()
                .unwrap()
                .unwrap()
                .subagents
                .contains_key("external-reviewer")
        );
        assert!(
            super::super::domains::load_extensions()
                .unwrap()
                .unwrap()
                .extensions
                .contains_key("external")
        );
        assert!(
            super::super::domains::load_providers()
                .unwrap()
                .unwrap()
                .providers
                .contains_key("external")
        );

        unsafe {
            match old_bone {
                Some(value) => std::env::set_var("BONE_DIR", value),
                None => std::env::remove_var("BONE_DIR"),
            }
        }
    }

    #[test]
    fn successful_mutation_refreshes_attached_compatibility_snapshot() {
        let _guard = crate::util::test_env_lock();
        let old_bone = std::env::var_os("BONE_DIR");
        let dir = std::env::temp_dir().join(format!(
            "bone-attached-config-snapshot-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        unsafe { std::env::set_var("BONE_DIR", &dir) };

        let initial = crate::ext::ExtensionManager::unloaded();
        let store = ConfigStore::new(initial.clone()).unwrap();
        let attached = crate::ext::ExtensionManager::unloaded();
        store.attach_extensions(attached.clone());
        let revision = store.snapshot().revision;
        store
            .set_value("general.show_reasoning", true.into(), revision)
            .unwrap();

        for extensions in [initial, attached] {
            assert_eq!(
                extensions
                    .config_snapshot()
                    .get_value("general", "show_thinking"),
                "true"
            );
        }

        std::fs::remove_dir_all(dir).ok();
        unsafe {
            match old_bone {
                Some(value) => std::env::set_var("BONE_DIR", value),
                None => std::env::remove_var("BONE_DIR"),
            }
        }
    }

    fn test_subagent(name: &str) -> bone_protocol::SubagentDefinition {
        bone_protocol::SubagentDefinition {
            name: name.into(),
            description: "Test agent".into(),
            approval: "safe".into(),
            enabled: true,
            source: "config".into(),
            ..Default::default()
        }
    }

    #[test]
    fn subagent_mutations_persist_and_keep_aggregate_mirrors_in_sync() {
        let _guard = crate::util::test_env_lock();
        let old_bone = std::env::var_os("BONE_DIR");
        let dir = std::env::temp_dir().join(format!(
            "bone-subagent-config-store-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        unsafe { std::env::set_var("BONE_DIR", &dir) };

        let extensions = crate::ext::ExtensionManager::unloaded();
        let store = ConfigStore::new(extensions.clone()).unwrap();
        let initial_revision = store.snapshot().revision;

        store
            .upsert_subagent(test_subagent("reviewer"), initial_revision)
            .unwrap();
        let after_upsert = store.snapshot();
        assert_eq!(after_upsert.revision, initial_revision + 1);
        assert!(after_upsert.values["subagents"]["reviewer"]["enabled"] == true);
        assert_eq!(extensions.subagents(), vec![test_subagent("reviewer")]);
        let persisted = Settings::load().unwrap().unwrap();
        assert!(persisted.resolved().subagents.is_empty());
        let persisted_subagents = super::super::domains::load_subagents()
            .unwrap()
            .unwrap()
            .subagents;
        assert!(persisted_subagents.contains_key("reviewer"));
        let root_yaml = std::fs::read_to_string(super::super::settings::settings_path()).unwrap();
        assert!(!root_yaml.contains("subagents:"));
        assert!(!root_yaml.contains("extensions:"));

        store
            .set_subagent_enabled("reviewer", false, after_upsert.revision)
            .unwrap();
        let after_disable = store.snapshot();
        assert_eq!(after_disable.revision, initial_revision + 2);
        assert!(after_disable.values["subagents"]["reviewer"]["enabled"] == false);
        assert!(!extensions.subagents()[0].enabled);

        store
            .delete_subagent("reviewer", after_disable.revision)
            .unwrap();
        let after_delete = store.snapshot();
        assert_eq!(after_delete.revision, initial_revision + 3);
        assert!(after_delete.values["subagents"]["reviewer"].is_null());
        assert!(extensions.subagents().is_empty());
        assert!(
            super::super::domains::load_subagents()
                .unwrap()
                .unwrap()
                .subagents
                .is_empty()
        );

        std::fs::remove_dir_all(dir).ok();
        unsafe {
            match old_bone {
                Some(value) => std::env::set_var("BONE_DIR", value),
                None => std::env::remove_var("BONE_DIR"),
            }
        }
    }

    #[test]
    fn failed_subagent_persistence_keeps_revision_and_typed_state() {
        let _guard = crate::util::test_env_lock();
        let old_bone = std::env::var_os("BONE_DIR");
        let dir = std::env::temp_dir().join(format!(
            "bone-failed-subagent-config-store-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        unsafe { std::env::set_var("BONE_DIR", &dir) };

        let extensions = crate::ext::ExtensionManager::unloaded();
        let store = ConfigStore::new(extensions.clone()).unwrap();
        let blocked = super::super::domains::subagents_path();
        std::fs::remove_file(&blocked).unwrap();
        std::fs::create_dir(&blocked).unwrap();
        let before = store.snapshot();
        let error = store
            .upsert_subagent(test_subagent("reviewer"), before.revision)
            .unwrap_err();

        let after = store.snapshot();
        assert_eq!(after.revision, before.revision);
        assert_eq!(after.values["subagents"], before.values["subagents"]);
        assert!(extensions.subagents().is_empty());
        assert!(error.1.contains(&blocked.display().to_string()));

        std::fs::remove_dir_all(dir).ok();
        unsafe {
            match old_bone {
                Some(value) => std::env::set_var("BONE_DIR", value),
                None => std::env::remove_var("BONE_DIR"),
            }
        }
    }
}

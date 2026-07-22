//! One-time migration from legacy page-shaped configuration to peer domain files.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde::Deserialize;

use super::custom::{ConfigFieldType, CustomConfigPage};
use super::domains::{ExtensionsConfig, SubagentsConfig};
use super::settings::{ExtensionValue, Settings};
use super::{ProviderCredential, ProviderEntry, ProvidersConfig, bone_dir, load_yaml};

pub(crate) const MIGRATION_VERSION: u8 = 1;
const MARKER: &str = ".config-layout-v1-migrated";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DenyListPage {
    #[allow(dead_code)]
    title: String,
    #[serde(default)]
    disabled: Vec<String>,
}

#[derive(Deserialize)]
struct LegacyProvidersConfig {
    #[serde(default, alias = "active")]
    last_provider: String,
    #[serde(default)]
    providers: HashMap<String, ProviderEntry>,
}

pub(crate) fn migrate() -> Result<(), String> {
    let root = bone_dir();
    std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    let lock_path = root.join(".config-migration.lock");
    let lock = File::options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|error| format!("cannot open {}: {error}", lock_path.display()))?;
    lock.lock_exclusive()
        .map_err(|error| format!("cannot lock {}: {error}", lock_path.display()))?;

    let marker = root.join(MARKER);
    if marker.exists() {
        return validate_marker(&marker);
    }

    // Build and validate the complete candidate before creating backups or
    // touching any destination.
    let legacy = LegacyInputs::load(&root)?;
    let candidate = Candidate::build(&legacy)?;
    candidate.validate()?;

    let stamp = timestamp();
    backup_sources(&legacy.sources, &stamp)?;

    #[cfg(test)]
    run_before_write_hook();
    write_candidate_and_marker(&candidate, &legacy, &marker)
}

#[cfg(test)]
type BeforeWriteHook = Box<dyn FnOnce() + Send>;
#[cfg(test)]
static BEFORE_WRITE_HOOK: std::sync::Mutex<Option<BeforeWriteHook>> = std::sync::Mutex::new(None);

#[cfg(test)]
fn run_before_write_hook() {
    if let Some(hook) = BEFORE_WRITE_HOOK.lock().unwrap().take() {
        hook();
    }
}

fn write_candidate_and_marker(
    candidate: &Candidate,
    legacy: &LegacyInputs,
    marker: &Path,
) -> Result<(), String> {
    candidate.write(legacy)?;
    write_bytes(
        marker,
        format!("version: {MIGRATION_VERSION}\n").as_bytes(),
        None,
    )
}

fn validate_marker(path: &Path) -> Result<(), String> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Marker {
        version: u8,
    }
    let marker: Marker = load_yaml(path)?;
    if marker.version == MIGRATION_VERSION {
        Ok(())
    } else {
        Err(format!(
            "unsupported config migration version {} in {}; expected {MIGRATION_VERSION}",
            marker.version,
            path.display()
        ))
    }
}

struct LegacyInputs {
    root_settings: Option<Settings>,
    root_yaml: Option<serde_yaml::Value>,
    pages: BTreeMap<String, CustomConfigPage>,
    disabled_tools: Vec<String>,
    disabled_commands: Vec<String>,
    providers: Option<ProvidersConfig>,
    command_policy_exists: bool,
    sources: Vec<PathBuf>,
}

impl LegacyInputs {
    fn load(root: &Path) -> Result<Self, String> {
        let root_path = root.join("config.yaml");
        let (root_settings, root_yaml) = if root_path.exists() {
            let settings = Settings::load()
                .map_err(|error| format!("cannot migrate {}: {error}", root_path.display()))?
                .ok_or_else(|| {
                    format!("cannot migrate {}: file disappeared", root_path.display())
                })?;
            let yaml = load_yaml(&root_path)?;
            (Some(settings), Some(yaml))
        } else {
            (None, None)
        };

        let legacy_dir = root.join("config");
        let mut pages = BTreeMap::new();
        let mut disabled_tools = Vec::new();
        let mut disabled_commands = Vec::new();
        let mut providers = None;
        let mut sources = Vec::new();
        if root_path.exists() {
            sources.push(root_path);
        }

        if legacy_dir.is_dir() {
            let mut entries = std::fs::read_dir(&legacy_dir)
                .map_err(|error| format!("cannot read {}: {error}", legacy_dir.display()))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("cannot read {}: {error}", legacy_dir.display()))?;
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let path = entry.path();
                if path.extension().and_then(|value| value.to_str()) != Some("yaml") {
                    continue;
                }
                let namespace = path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .ok_or_else(|| format!("invalid legacy config filename: {}", path.display()))?
                    .to_string();
                sources.push(path.clone());
                match namespace.as_str() {
                    "tools" => disabled_tools = load_deny_list(&path)?,
                    "commands" => disabled_commands = load_deny_list(&path)?,
                    "providers" => providers = Some(load_legacy_providers(&path)?),
                    _ => {
                        pages.insert(namespace, load_yaml(&path)?);
                    }
                }
            }
        }

        let values_path = root.join("config-values.yaml");
        if values_path.exists() {
            let values: BTreeMap<String, BTreeMap<String, String>> = load_yaml(&values_path)?;
            apply_old_values(&mut pages, &values);
            sources.push(values_path);
        }
        move_general_status_values(&mut pages);
        validate_legacy_page_values(&pages)?;

        let command_policy = root.join("command-policy.yaml");
        let command_policy_exists = command_policy.exists();
        if command_policy_exists {
            crate::tools::command_policy::validate_command_policy_path(&command_policy)
                .map_err(|error| error.to_string())?;
        }
        for path in [
            root.join("providers.yaml"),
            root.join("subagents.yaml"),
            root.join("extensions.yaml"),
            command_policy,
        ] {
            if path.exists() {
                sources.push(path);
            }
        }

        sources.sort();
        sources.dedup();

        Ok(Self {
            root_settings,
            root_yaml,
            pages,
            disabled_tools,
            disabled_commands,
            providers,
            command_policy_exists,
            sources,
        })
    }
}

struct Candidate {
    core: Settings,
    providers: ProvidersConfig,
    subagents: SubagentsConfig,
    extensions: ExtensionsConfig,
    write_providers: bool,
    write_subagents: bool,
    write_extensions: bool,
    write_command_policy: bool,
}

impl Candidate {
    fn build(legacy: &LegacyInputs) -> Result<Self, String> {
        let value_pages = pages_with_values_only(&legacy.pages);
        let mut core = legacy.root_settings.clone().unwrap_or_else(|| {
            Settings::migrate_from_pages(&value_pages.clone().into_iter().collect::<Vec<_>>())
        });
        if let (Some(yaml), Some(root_settings)) = (&legacy.root_yaml, &legacy.root_settings) {
            fill_missing_page_values(&mut core, yaml, root_settings, &value_pages);
        }
        core.inner.version = 2;
        if !yaml_has(legacy.root_yaml.as_ref(), &["tools"]) {
            core.inner.tools.disabled = legacy.disabled_tools.clone();
        }
        if !yaml_has(legacy.root_yaml.as_ref(), &["commands"]) {
            core.inner.commands.disabled = legacy.disabled_commands.clone();
        }

        let (providers, write_providers) = match super::domains::load_providers()? {
            Some(config) => (config, false),
            None => (legacy.providers.clone().unwrap_or_default(), true),
        };
        // For a fresh install (no legacy providers and no existing providers.yaml),
        // seed well-known provider templates so the onboarding wizard has entries
        // to show and the app remains launchable after setup completes.
        let providers = if write_providers && providers.providers.is_empty() {
            let mut seeded = ProvidersConfig::default();
            seeded.providers.insert(
                "local".into(),
                ProviderEntry {
                    label: "llama.cpp".into(),
                    base_url: "http://127.0.0.1:8080".into(),
                    model: "local".into(),
                    api_key: ProviderCredential::from(""),
                    endpoint: "/v1/chat/completions".into(),
                    handler: "openai".into(),
                    context_window_tokens: None,
                    reasoning_effort: String::new(),
                },
            );
            seeded.providers.insert(
                "openai".into(),
                ProviderEntry {
                    label: "OpenAI".into(),
                    base_url: "https://api.openai.com/v1".into(),
                    model: "gpt-4o".into(),
                    api_key: ProviderCredential::from(""),
                    endpoint: "/chat/completions".into(),
                    handler: "openai".into(),
                    context_window_tokens: None,
                    reasoning_effort: String::new(),
                },
            );
            seeded.providers.insert(
                "anthropic".into(),
                ProviderEntry {
                    label: "Anthropic".into(),
                    base_url: "https://api.anthropic.com".into(),
                    model: "claude-sonnet-4-20250514".into(),
                    api_key: ProviderCredential::from(""),
                    endpoint: "/messages".into(),
                    handler: "anthropic".into(),
                    context_window_tokens: None,
                    reasoning_effort: String::new(),
                },
            );
            seeded
        } else {
            providers
        };
        let (subagents, write_subagents) = match super::domains::load_subagents()? {
            Some(config) => (config, false),
            None => (
                SubagentsConfig {
                    version: 1,
                    subagents: core.resolved().subagents.clone(),
                },
                true,
            ),
        };
        let (mut extensions, write_extensions) = match super::domains::load_extensions()? {
            Some(config) => (config, false),
            None => (
                ExtensionsConfig {
                    version: 1,
                    extensions: core.resolved().extensions.clone(),
                },
                true,
            ),
        };
        if write_extensions {
            migrate_compaction_values(&legacy.pages, &mut extensions.extensions);
        }

        core.replace_domains(BTreeMap::new(), BTreeMap::new());
        Ok(Self {
            core,
            providers,
            subagents,
            extensions,
            write_providers,
            write_subagents,
            write_extensions,
            write_command_policy: !legacy.command_policy_exists,
        })
    }

    fn validate(&self) -> Result<(), String> {
        self.core.validate().map_err(|error| error.to_string())?;
        super::domains::validate_providers(&self.providers)?;
        super::settings::validate_subagents(&self.subagents.subagents)
            .map_err(|error| error.to_string())?;
        for (name, agent) in &self.subagents.subagents {
            if let Some(provider) = agent.provider.as_deref()
                && !self.providers.providers.contains_key(provider)
            {
                return Err(format!(
                    "subagents.{name}.provider references unknown provider {provider:?}"
                ));
            }
        }
        Ok(())
    }

    fn write(&self, legacy: &LegacyInputs) -> Result<(), String> {
        let root = bone_dir();
        if self.write_providers {
            super::domains::write_document(
                &root.join("providers.yaml"),
                &self.providers,
                legacy
                    .sources
                    .iter()
                    .find(|path| path.ends_with("config/providers.yaml"))
                    .map(PathBuf::as_path),
            )?;
        }
        if self.write_subagents {
            super::domains::write_document(
                &root.join("subagents.yaml"),
                &self.subagents,
                legacy
                    .root_settings
                    .as_ref()
                    .map(|_| root.join("config.yaml"))
                    .as_deref(),
            )?;
        }
        if self.write_extensions {
            super::domains::write_document(
                &root.join("extensions.yaml"),
                &self.extensions,
                legacy
                    .root_settings
                    .as_ref()
                    .map(|_| root.join("config.yaml"))
                    .as_deref(),
            )?;
        }
        if self.write_command_policy {
            let path = root.join("command-policy.yaml");
            write_bytes(&path, super::DEFAULT_COMMAND_POLICY.as_bytes(), None)?;
            crate::tools::command_policy::validate_command_policy_path(&path)
                .map_err(|error| error.to_string())?;
        }
        // Write config.yaml after every extracted domain. If interrupted after
        // this point, a retry can safely use the already-written peer files.
        let config_path = root.join("config.yaml");
        let permissions = legacy
            .root_settings
            .as_ref()
            .and_then(|_| std::fs::metadata(&config_path).ok())
            .map(|metadata| metadata.permissions());
        let yaml = self.core.sparse_yaml().map_err(|error| error.to_string())?;
        write_bytes(&config_path, yaml.as_bytes(), permissions)
    }
}

fn load_deny_list(path: &Path) -> Result<Vec<String>, String> {
    if let Ok(page) = load_yaml::<DenyListPage>(path) {
        return Ok(page.disabled);
    }
    let page: CustomConfigPage = load_yaml(path)?;
    Ok(page
        .fields
        .into_iter()
        .filter(|field| field.value == Some(serde_yaml::Value::Bool(false)))
        .map(|field| field.key)
        .collect())
}

fn load_legacy_providers(path: &Path) -> Result<ProvidersConfig, String> {
    if let Ok(page) = load_yaml::<CustomConfigPage>(path) {
        let mut config = ProvidersConfig::default();
        for field in page.fields {
            if field.key == "_last_provider" {
                config.last_provider = match field.value {
                    Some(serde_yaml::Value::String(value)) => value,
                    Some(value) => {
                        return Err(format!(
                            "parse error in {}: providers._last_provider must be a string, got {value:?}",
                            path.display()
                        ));
                    }
                    None => String::new(),
                };
                continue;
            }
            if field.field_type != ConfigFieldType::Provider {
                return Err(format!(
                    "parse error in {}: providers.{} must have type provider",
                    path.display(),
                    field.key
                ));
            }
            let value = field.value.ok_or_else(|| {
                format!(
                    "parse error in {}: providers.{} has no value",
                    path.display(),
                    field.key
                )
            })?;
            let entry = serde_yaml::from_value(value).map_err(|error| {
                format!(
                    "parse error in {} at providers.{}: {error}",
                    path.display(),
                    field.key
                )
            })?;
            config.providers.insert(field.key, entry);
        }
        return Ok(config);
    }
    let legacy: LegacyProvidersConfig = load_yaml(path)?;
    Ok(ProvidersConfig {
        version: 1,
        last_provider: legacy.last_provider,
        providers: legacy.providers,
    })
}

fn apply_old_values(
    pages: &mut BTreeMap<String, CustomConfigPage>,
    values: &BTreeMap<String, BTreeMap<String, String>>,
) {
    for (namespace, fields) in values {
        let Some(page) = pages.get_mut(namespace) else {
            continue;
        };
        for field in &mut page.fields {
            if let Some(value) = fields.get(&field.key) {
                field.value = Some(value_for_field(field, value.clone()));
            }
        }
    }
}

fn move_general_status_values(pages: &mut BTreeMap<String, CustomConfigPage>) {
    let Some(general) = pages.get("general") else {
        return;
    };
    let values: BTreeMap<_, _> = general
        .fields
        .iter()
        .filter_map(|field| field.value.clone().map(|value| (field.key.clone(), value)))
        .collect();
    let Some(status) = pages.get_mut("status") else {
        return;
    };
    for field in &mut status.fields {
        if field.value.is_none()
            && super::UserConfig::STATUS_TOGGLE_KEYS.contains(&field.key.as_str())
            && let Some(value) = values.get(&field.key)
        {
            field.value = Some(value.clone());
        }
    }
}

fn validate_legacy_page_values(pages: &BTreeMap<String, CustomConfigPage>) -> Result<(), String> {
    for namespace in ["general", "status"] {
        let Some(page) = pages.get(namespace) else {
            continue;
        };
        for field in &page.fields {
            let Some(value) = &field.value else {
                continue;
            };
            let valid = match field.field_type {
                ConfigFieldType::String => value.is_string(),
                ConfigFieldType::Number => value.is_number(),
                ConfigFieldType::Bool => value.is_bool(),
                ConfigFieldType::Enum => value.as_str().is_some_and(|value| {
                    field.options.is_empty() || field.options.iter().any(|option| option == value)
                }),
                ConfigFieldType::Provider => false,
            };
            if !valid {
                return Err(format!(
                    "invalid explicit legacy value at {namespace}.{}: {value:?}",
                    field.key
                ));
            }
        }
    }
    Ok(())
}

fn pages_with_values_only(
    pages: &BTreeMap<String, CustomConfigPage>,
) -> BTreeMap<String, CustomConfigPage> {
    let mut pages = pages.clone();
    for page in pages.values_mut() {
        for field in &mut page.fields {
            field.default = None;
        }
    }
    pages
}

fn fill_missing_page_values(
    core: &mut Settings,
    yaml: &serde_yaml::Value,
    root: &Settings,
    pages: &BTreeMap<String, CustomConfigPage>,
) {
    let page = Settings::migrate_from_pages(&pages.clone().into_iter().collect::<Vec<_>>());
    if !yaml_has(Some(yaml), &["general", "approval"]) {
        core.inner.general.approval = page.inner.general.approval;
    }
    if !yaml_has(Some(yaml), &["general", "show_reasoning"]) {
        core.inner.general.show_reasoning = page.inner.general.show_reasoning;
    }
    if !yaml_has(Some(yaml), &["ui", "input", "preset"]) {
        core.inner.ui.input.preset = page.inner.ui.input.preset;
    }
    macro_rules! fill_ui {
        ($field:ident) => {
            if !yaml_has(Some(yaml), &["ui", stringify!($field)]) {
                core.inner.ui.$field = page.inner.ui.$field;
            }
        };
    }
    fill_ui!(status_show_model);
    fill_ui!(status_show_approval);
    fill_ui!(status_show_tokens_curr);
    fill_ui!(status_show_tokens_in);
    fill_ui!(status_show_tokens_out);
    fill_ui!(status_show_tokens_total);
    fill_ui!(status_show_queue);
    fill_ui!(status_show_spinner);
    fill_ui!(status_show_timer);
    fill_ui!(spinner_style);
    fill_ui!(spinner_text);
    fill_ui!(spinner_custom);
    fill_ui!(spinner_speed);
    fill_ui!(spinner_text_rotate);
    fill_ui!(spinner_text_speed);
    core.inner.subagents = root.inner.subagents.clone();
    core.inner.extensions = root.inner.extensions.clone();
}

fn migrate_compaction_values(
    pages: &BTreeMap<String, CustomConfigPage>,
    extensions: &mut BTreeMap<String, BTreeMap<String, ExtensionValue>>,
) {
    let Some(page) = pages.get("general") else {
        rename_compaction_fallback(extensions);
        return;
    };
    let value = |key: &str| {
        page.fields
            .iter()
            .find(|field| field.key == key)
            .and_then(|field| field.value.as_ref())
            .and_then(yaml_scalar_string)
    };
    let has_legacy = page.fields.iter().any(|field| {
        field.value.is_some()
            && (field.key.starts_with("compact_") || field.key == "auto_compact_tokens")
    });
    if has_legacy {
        let mode = value("compact_trigger_mode").unwrap_or_default();
        let auto_tokens = value("auto_compact_tokens")
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        let compact = extensions.entry("compact".into()).or_default();
        compact.entry("auto".into()).or_insert(ExtensionValue::Bool(
            mode == "percentage" || auto_tokens > 0,
        ));
        if let Some(value) =
            value("compact_trigger_percentage").and_then(|value| value.parse::<f64>().ok())
        {
            compact
                .entry("trigger_percentage".into())
                .or_insert(ExtensionValue::Number(value));
        }
        if let Some(value) =
            value("compact_context_window_tokens").and_then(|value| value.parse::<f64>().ok())
        {
            compact
                .entry("context_window_tokens".into())
                .or_insert(ExtensionValue::Number(value));
        }
    }
    rename_compaction_fallback(extensions);
}

fn rename_compaction_fallback(extensions: &mut BTreeMap<String, BTreeMap<String, ExtensionValue>>) {
    let Some(compact) = extensions.get_mut("compact") else {
        return;
    };
    if let Some(value) = compact.remove("fallback_context_window_tokens") {
        compact
            .entry("context_window_tokens".into())
            .or_insert(value);
    }
}

fn yaml_scalar_string(value: &serde_yaml::Value) -> Option<String> {
    match value {
        serde_yaml::Value::String(value) => Some(value.clone()),
        serde_yaml::Value::Number(value) => Some(value.to_string()),
        serde_yaml::Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn value_for_field(field: &super::custom::ConfigField, value: String) -> serde_yaml::Value {
    match field.field_type {
        ConfigFieldType::Bool => match value.as_str() {
            "true" => serde_yaml::Value::Bool(true),
            "false" => serde_yaml::Value::Bool(false),
            _ => serde_yaml::Value::String(value),
        },
        ConfigFieldType::Number => value
            .parse::<serde_yaml::Number>()
            .map(serde_yaml::Value::Number)
            .unwrap_or(serde_yaml::Value::String(value)),
        _ => serde_yaml::Value::String(value),
    }
}

fn yaml_has(root: Option<&serde_yaml::Value>, path: &[&str]) -> bool {
    let Some(mut value) = root else {
        return false;
    };
    for key in path {
        let Some(next) = value.as_mapping().and_then(|mapping| mapping.get(*key)) else {
            return false;
        };
        value = next;
    }
    true
}

fn timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos().to_string())
        .unwrap_or_else(|_| format!("{}", std::process::id()))
}

fn backup_sources(sources: &[PathBuf], stamp: &str) -> Result<(), String> {
    let mut sources = sources.to_vec();
    sources.sort();
    sources.dedup();
    for source in sources {
        backup(&source, stamp)?;
    }
    Ok(())
}

fn backup(path: &Path, stamp: &str) -> Result<(), String> {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| format!("cannot back up invalid path {}", path.display()))?;
    let backup = path.with_file_name(format!("{name}.bak.{stamp}"));
    let bytes = std::fs::read(path)
        .map_err(|error| format!("cannot read {} for backup: {error}", path.display()))?;
    let permissions = std::fs::metadata(path)
        .map_err(|error| format!("cannot inspect {} for backup: {error}", path.display()))?
        .permissions();
    write_bytes(&backup, &bytes, Some(permissions))
}

fn write_bytes(
    path: &Path,
    bytes: &[u8],
    permissions: Option<std::fs::Permissions>,
) -> Result<(), String> {
    crate::tools::write_atomic::write_atomic_sync(path, bytes, permissions)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestRoot {
        old_bone: Option<std::ffi::OsString>,
        dir: tempfile::TempDir,
    }

    impl TestRoot {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let old_bone = std::env::var_os("BONE_DIR");
            unsafe { std::env::set_var("BONE_DIR", dir.path()) };
            Self { old_bone, dir }
        }

        fn path(&self, relative: &str) -> PathBuf {
            self.dir.path().join(relative)
        }

        fn write(&self, relative: &str, contents: &str) {
            let path = self.path(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, contents).unwrap();
        }

        fn backups(&self, relative: &str) -> Vec<PathBuf> {
            let path = self.path(relative);
            let prefix = format!("{}.bak.", path.file_name().unwrap().to_string_lossy());
            let mut found = std::fs::read_dir(path.parent().unwrap())
                .unwrap()
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|candidate| {
                    candidate
                        .file_name()
                        .is_some_and(|name| name.to_string_lossy().starts_with(&prefix))
                })
                .collect::<Vec<_>>();
            found.sort();
            found
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            unsafe {
                match self.old_bone.take() {
                    Some(value) => std::env::set_var("BONE_DIR", value),
                    None => std::env::remove_var("BONE_DIR"),
                }
            }
        }
    }

    #[test]
    fn invalid_input_creates_no_backups_destinations_or_marker() {
        let _guard = crate::util::test_env_lock();
        let root = TestRoot::new();
        root.write(
            "config/general.yaml",
            "title: General\nfields:\n  - key: show_thinking\n    type: bool\n    value: not-a-bool\n",
        );

        let error = migrate().unwrap_err();

        assert!(error.contains("invalid explicit legacy value"));
        assert!(root.backups("config/general.yaml").is_empty());
        for destination in [
            "config.yaml",
            "providers.yaml",
            "subagents.yaml",
            "extensions.yaml",
            "command-policy.yaml",
            MARKER,
        ] {
            assert!(!root.path(destination).exists(), "unexpected {destination}");
        }
        assert!(root.path("config/general.yaml").exists());
        assert!(
            root.path(".config-migration.lock").exists(),
            "the coordination lock is allowed; only migration outputs are forbidden"
        );
    }

    #[test]
    fn migration_preserves_precedence_credentials_permissions_and_is_idempotent() {
        let _guard = crate::util::test_env_lock();
        let root = TestRoot::new();
        root.write(
            "config.yaml",
            r#"version: 1
general:
  approval: safe
subagents:
  reviewer:
    description: "Review exactly"
    system_prompt: "keep\nthis prompt"
    provider: env
    approval: danger
    enabled: false
extensions:
  compact:
    fallback_context_window_tokens: 12345
"#,
        );
        root.write(
            "config/general.yaml",
            r#"title: General
fields:
  - key: approval_mode
    type: enum
    options: [safe, danger]
    default: danger
    value: danger
  - key: show_thinking
    type: bool
    default: false
    value: true
"#,
        );
        root.write(
            "config/status.yaml",
            r#"title: Status
fields:
  - key: status_show_model
    type: bool
    default: false
  - key: status_show_timer
    type: bool
    default: true
    value: false
"#,
        );
        root.write("config/tools.yaml", "title: Tools\ndisabled: [shell]\n");
        root.write(
            "config/commands.yaml",
            "title: Commands\ndisabled: [history]\n",
        );
        root.write(
            "config/providers.yaml",
            r#"title: Providers
fields:
  - key: env
    type: provider
    value:
      label: Environment
      base_url: https://example.test
      model: exact-model
      api_key: '${BONE_SECRET}'
      endpoint: /v1/chat
      handler: openai
  - key: plain
    type: provider
    value:
      label: Plain
      base_url: http://localhost
      model: local
      api_key: ' plaintext $KEY '
      endpoint: /chat
      handler: openai
  - key: _last_provider
    type: string
    default: plain
    value: env
"#,
        );
        let policy = "shell_wrappers: [bash]\nread_only: [pwd]\nedit: [rm]\npackage_managers: []\n";
        root.write("command-policy.yaml", policy);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                root.path("config.yaml"),
                std::fs::Permissions::from_mode(0o640),
            )
            .unwrap();
            std::fs::set_permissions(
                root.path("config/providers.yaml"),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }

        migrate().unwrap();

        let core: super::super::settings::BoneSettings =
            load_yaml(&root.path("config.yaml")).unwrap();
        assert_eq!(core.version, 2);
        assert_eq!(core.general.approval, "safe");
        assert!(core.general.show_reasoning);
        assert!(core.ui.status_show_model, "page defaults must not migrate");
        assert!(!core.ui.status_show_timer);
        assert_eq!(core.tools.disabled, ["shell"]);
        assert_eq!(core.commands.disabled, ["history"]);
        let core_text = std::fs::read_to_string(root.path("config.yaml")).unwrap();
        assert!(!core_text.contains("subagents:"));
        assert!(!core_text.contains("extensions:"));

        let providers: ProvidersConfig = load_yaml(&root.path("providers.yaml")).unwrap();
        assert_eq!(providers.last_provider, "env");
        assert_eq!(
            providers.providers["env"].api_key.as_str(),
            "${BONE_SECRET}"
        );
        assert_eq!(
            providers.providers["plain"].api_key.as_str(),
            " plaintext $KEY "
        );
        let subagents: SubagentsConfig = load_yaml(&root.path("subagents.yaml")).unwrap();
        let reviewer = &subagents.subagents["reviewer"];
        assert_eq!(reviewer.system_prompt.as_deref(), Some("keep\nthis prompt"));
        assert!(!reviewer.enabled);
        let extensions: ExtensionsConfig = load_yaml(&root.path("extensions.yaml")).unwrap();
        assert_eq!(
            extensions.extensions["compact"]["context_window_tokens"],
            ExtensionValue::Number(12345.0)
        );
        assert_eq!(
            std::fs::read_to_string(root.path("command-policy.yaml")).unwrap(),
            policy
        );
        crate::tools::command_policy::validate_command_policy_path(
            &root.path("command-policy.yaml"),
        )
        .unwrap();
        assert!(root.path(MARKER).exists());

        for source in [
            "config.yaml",
            "config/general.yaml",
            "config/status.yaml",
            "config/tools.yaml",
            "config/commands.yaml",
            "config/providers.yaml",
            "command-policy.yaml",
        ] {
            assert_eq!(root.backups(source).len(), 1, "backup for {source}");
            assert!(root.path(source).exists(), "retained source {source}");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(root.path("config.yaml"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o640
            );
            assert_eq!(
                std::fs::metadata(root.path("providers.yaml"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(&root.backups("config/providers.yaml")[0])
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }

        let before = std::fs::read(root.path("config.yaml")).unwrap();
        migrate().unwrap();
        assert_eq!(std::fs::read(root.path("config.yaml")).unwrap(), before);
        assert_eq!(root.backups("config.yaml").len(), 1);
    }

    #[test]
    fn existing_peer_documents_win_and_generated_policy_is_valid() {
        let _guard = crate::util::test_env_lock();
        let root = TestRoot::new();
        root.write(
            "config.yaml",
            "version: 1\nsubagents:\n  old:\n    description: old\n",
        );
        root.write(
            "config/providers.yaml",
            "last_provider: legacy\nproviders:\n  legacy:\n    label: Legacy\n",
        );
        let providers = "version: 1\nactive: current\nproviders:\n  current:\n    label: Current\n    api_key: current-secret\n";
        let subagents = "version: 1\nsubagents:\n  current:\n    description: Current\n";
        let extensions = "version: 1\nextensions:\n  absent_plugin:\n    retained: exact\n";
        root.write("providers.yaml", providers);
        root.write("subagents.yaml", subagents);
        root.write("extensions.yaml", extensions);

        migrate().unwrap();

        assert_eq!(
            std::fs::read_to_string(root.path("providers.yaml")).unwrap(),
            providers
        );
        assert_eq!(
            std::fs::read_to_string(root.path("subagents.yaml")).unwrap(),
            subagents
        );
        assert_eq!(
            std::fs::read_to_string(root.path("extensions.yaml")).unwrap(),
            extensions
        );
        assert!(
            !std::fs::read_to_string(root.path("config.yaml"))
                .unwrap()
                .contains("subagents:")
        );
        crate::tools::command_policy::validate_command_policy_path(
            &root.path("command-policy.yaml"),
        )
        .unwrap();
        assert!(root.path(MARKER).exists());
        assert_eq!(root.backups("providers.yaml").len(), 1);
    }

    #[test]
    fn duplicate_backups_are_deduplicated_and_marker_is_written_last() {
        let _guard = crate::util::test_env_lock();
        let root = TestRoot::new();
        root.write("source.yaml", "exact\n");
        let source = root.path("source.yaml");
        backup_sources(&[source.clone(), source.clone()], "same").unwrap();
        assert_eq!(root.backups("source.yaml").len(), 1);

        let legacy = LegacyInputs {
            root_settings: None,
            root_yaml: None,
            pages: BTreeMap::new(),
            disabled_tools: Vec::new(),
            disabled_commands: Vec::new(),
            providers: None,
            command_policy_exists: false,
            sources: Vec::new(),
        };
        let candidate = Candidate::build(&legacy).unwrap();
        std::fs::create_dir(root.path("config.yaml")).unwrap();
        let marker = root.path(MARKER);
        assert!(write_candidate_and_marker(&candidate, &legacy, &marker).is_err());
        assert!(!marker.exists());
        assert!(root.path("providers.yaml").exists());
        assert!(root.path("subagents.yaml").exists());
        assert!(root.path("extensions.yaml").exists());
    }

    #[test]
    fn old_values_and_flat_providers_migrate_without_deleting_sources() {
        let _guard = crate::util::test_env_lock();
        let root = TestRoot::new();
        let values = r#"general:
  approval_mode: danger
  show_thinking: "true"
status:
  status_show_timer: "false"
"#;
        let providers = r#"last_provider: flat
providers:
  flat:
    label: Flat
    base_url: https://flat.example
    model: flat-model
    api_key: ' exact-flat-secret '
    endpoint: /v1/chat
    handler: openai
"#;
        root.write(
            "config/general.yaml",
            "title: General\nfields:\n  - key: approval_mode\n    type: enum\n    options: [safe, danger]\n  - key: show_thinking\n    type: bool\n",
        );
        root.write(
            "config/status.yaml",
            "title: Status\nfields:\n  - key: status_show_timer\n    type: bool\n",
        );
        root.write("config-values.yaml", values);
        root.write("config/providers.yaml", providers);

        migrate().unwrap();

        let core: super::super::settings::BoneSettings =
            load_yaml(&root.path("config.yaml")).unwrap();
        assert_eq!(core.general.approval, "danger");
        assert!(core.general.show_reasoning);
        assert!(!core.ui.status_show_timer);
        let migrated: ProvidersConfig = load_yaml(&root.path("providers.yaml")).unwrap();
        assert_eq!(migrated.last_provider, "flat");
        assert_eq!(
            migrated.providers["flat"].api_key.as_str(),
            " exact-flat-secret "
        );
        for (source, exact) in [
            ("config-values.yaml", values),
            ("config/providers.yaml", providers),
        ] {
            assert_eq!(std::fs::read_to_string(root.path(source)).unwrap(), exact);
            assert_eq!(root.backups(source).len(), 1);
        }
    }

    #[test]
    fn interrupted_migrate_retries_after_partial_peer_writes() {
        let _guard = crate::util::test_env_lock();
        let root = TestRoot::new();
        let legacy = "last_provider: retained\nproviders:\n  retained:\n    label: Retained\n    api_key: ' exact '\n";
        root.write("config/providers.yaml", legacy);

        let obstruction = root.path("config.yaml");
        *BEFORE_WRITE_HOOK.lock().unwrap() = Some(Box::new(move || {
            std::fs::create_dir(&obstruction).unwrap();
        }));
        let error = migrate().unwrap_err();

        assert!(!error.is_empty());
        assert!(!root.path(MARKER).exists());
        for peer in [
            "providers.yaml",
            "subagents.yaml",
            "extensions.yaml",
            "command-policy.yaml",
        ] {
            assert!(root.path(peer).is_file(), "partial peer write {peer}");
        }
        assert_eq!(
            std::fs::read_to_string(root.path("config/providers.yaml")).unwrap(),
            legacy
        );
        assert!(!root.backups("config/providers.yaml").is_empty());

        std::fs::remove_dir(root.path("config.yaml")).unwrap();
        migrate().unwrap();

        assert!(root.path(MARKER).is_file());
        assert!(root.path("config.yaml").is_file());
        assert_eq!(
            std::fs::read_to_string(root.path("config/providers.yaml")).unwrap(),
            legacy
        );
        assert!(!root.backups("config/providers.yaml").is_empty());
        let migrated: ProvidersConfig = load_yaml(&root.path("providers.yaml")).unwrap();
        assert_eq!(migrated.last_provider, "retained");
        assert_eq!(migrated.providers["retained"].api_key.as_str(), " exact ");
    }

    #[test]
    fn invalid_marker_prevents_retry_writes() {
        let _guard = crate::util::test_env_lock();
        let root = TestRoot::new();
        root.write(MARKER, "version: 99\n");
        assert!(
            migrate()
                .unwrap_err()
                .contains("unsupported config migration version")
        );
        assert!(!root.path("config.yaml").exists());
        assert!(root.backups(MARKER).is_empty());
    }

    #[test]
    fn fresh_install_seeds_provider_templates() {
        let _guard = crate::util::test_env_lock();
        let root = TestRoot::new();
        // No legacy data at all — simulates a brand-new BONE_DIR.
        migrate().unwrap();

        let settings = std::fs::read_to_string(root.path("config.yaml")).unwrap();
        assert_eq!(settings, "version: 2\n");

        let providers: ProvidersConfig = load_yaml(&root.path("providers.yaml")).unwrap();
        assert_eq!(providers.version, 1);
        // last_provider must be empty — setup's provider step sets it.
        assert!(providers.last_provider.is_empty());
        // Must have the three seeded template providers.
        for id in ["local", "openai", "anthropic"] {
            let entry = providers
                .providers
                .get(id)
                .unwrap_or_else(|| panic!("fresh install must seed provider template {id:?}"));
            assert!(
                entry.api_key.is_empty(),
                "seeded provider {id:?} must have an empty api_key"
            );
            assert!(
                !entry.base_url.is_empty(),
                "seeded provider {id:?} must have a base_url"
            );
            assert!(
                !entry.model.is_empty(),
                "seeded provider {id:?} must have a model"
            );
        }
        // The local provider must match the canonical llama.cpp preset.
        let local = &providers.providers["local"];
        assert_eq!(local.label, "llama.cpp");
        assert_eq!(local.base_url, "http://127.0.0.1:8080");
        assert_eq!(local.model, "local");
        assert_eq!(local.endpoint, "/v1/chat/completions");
        assert_eq!(local.handler, "openai");
    }

    #[test]
    fn fresh_install_seeding_is_idempotent() {
        let _guard = crate::util::test_env_lock();
        let root = TestRoot::new();
        migrate().unwrap();
        let first: ProvidersConfig = load_yaml(&root.path("providers.yaml")).unwrap();
        // Second migration must preserve the seeded providers unchanged.
        migrate().unwrap();
        let second: ProvidersConfig = load_yaml(&root.path("providers.yaml")).unwrap();
        assert_eq!(first.providers.len(), second.providers.len());
        for id in ["local", "openai", "anthropic"] {
            assert_eq!(first.providers[id].base_url, second.providers[id].base_url);
        }
    }

    #[test]
    fn existing_peer_documents_suppress_seeding() {
        let _guard = crate::util::test_env_lock();
        let root = TestRoot::new();
        // Write an explicit providers.yaml (simulating a non-fresh user).
        root.write(
            "providers.yaml",
            "version: 1\nactive: custom\nproviders:\n  custom:\n    label: Custom\n    base_url: http://example.com\n    model: m\n    handler: openai\n",
        );
        migrate().unwrap();
        let providers: ProvidersConfig = load_yaml(&root.path("providers.yaml")).unwrap();
        // Must NOT contain seeded templates — the user's explicit config wins.
        assert_eq!(providers.providers.len(), 1);
        assert!(providers.providers.contains_key("custom"));
        assert!(!providers.providers.contains_key("local"));
    }
}

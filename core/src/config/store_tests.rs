use super::*;

#[test]
fn system_prompt_schema_mutation_reset_and_persistence() {
    let _guard = crate::util::test_env_lock();
    let previous = std::env::var_os("BONE_DIR");
    let dir = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("BONE_DIR", dir.path()) };

    let shipped = super::super::settings::shipped_system_prompt();
    let store = ConfigStore::new(crate::ext::ExtensionManager::unloaded()).unwrap();
    let schema = store.schema();
    let field = schema
        .pages
        .iter()
        .find(|page| page.namespace == "general")
        .and_then(|page| {
            page.fields
                .iter()
                .find(|field| field.path == "general.system_prompt")
        })
        .expect("general.system_prompt schema field");
    assert_eq!(field.value_type, "string");
    assert_eq!(field.default, serde_json::Value::from(shipped));

    let assert_value = |expected: &str| {
        let snapshot = store.snapshot();
        assert_eq!(
            snapshot.values["general"]["system_prompt"],
            serde_json::Value::from(expected)
        );
        assert_eq!(
            store
                .runtime_settings_snapshot()
                .resolved()
                .general
                .system_prompt
                .as_deref(),
            Some(expected)
        );
        assert_eq!(
            Settings::load()
                .unwrap()
                .unwrap()
                .resolved()
                .general
                .system_prompt
                .as_deref(),
            Some(expected)
        );
        assert!(
            std::fs::read_to_string(super::super::settings::settings_path())
                .unwrap()
                .contains("system_prompt:")
        );
    };

    assert_value(shipped);

    store
        .set_value(
            "general.system_prompt",
            serde_json::json!("Configured base prompt"),
            store.snapshot().revision,
        )
        .unwrap();
    assert_value("Configured base prompt");

    store
        .set_value(
            "general.system_prompt",
            serde_json::Value::Null,
            store.snapshot().revision,
        )
        .unwrap();
    assert_value(shipped);

    store
        .set_value(
            "general.system_prompt",
            serde_json::json!("Configured again"),
            store.snapshot().revision,
        )
        .unwrap();
    store
        .reset_value("general.system_prompt", store.snapshot().revision)
        .unwrap();
    assert_value(shipped);

    unsafe {
        match previous {
            Some(value) => std::env::set_var("BONE_DIR", value),
            None => std::env::remove_var("BONE_DIR"),
        }
    }
}

#[test]
fn provider_mutation_accepts_custom_reasoning_effort() {
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
        max_concurrency: None,
        reasoning_effort: "ultra".into(),
        fast_mode: None,
        supports_prompt_cache_key: Some(true),
        stream_usage: None,
        api_key: None,
    };

    store.upsert_provider(update, before.revision).unwrap();
    let after = store.snapshot();
    assert_eq!(after.revision, before.revision + 1);
    let provider = after
        .providers
        .iter()
        .find(|provider| provider.id == "test")
        .unwrap();
    assert_eq!(provider.reasoning_effort, "ultra");
    assert!(provider.supports_prompt_cache_key);
    let persisted = super::super::domains::load_providers().unwrap().unwrap();
    assert!(persisted.providers["test"].supports_prompt_cache_key);

    unsafe {
        match previous {
            Some(value) => std::env::set_var("BONE_DIR", value),
            None => std::env::remove_var("BONE_DIR"),
        }
    }
}

#[test]
fn provider_mutation_rejects_invalid_completed_candidates() {
    let _guard = crate::util::test_env_lock();
    let previous = std::env::var_os("BONE_DIR");
    let dir = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("BONE_DIR", dir.path()) };

    let store = ConfigStore::new(crate::ext::ExtensionManager::unloaded()).unwrap();
    for (id, max_concurrency, fast_mode, expected_error) in [
        (
            "zero-concurrency",
            Some(0),
            Some(false),
            "max_concurrency must be at least 1",
        ),
        (
            "non-codex-fast",
            None,
            Some(true),
            "fast_mode is only supported by the codex handler",
        ),
    ] {
        let before = store.snapshot();
        let update = ProviderUpdate {
            id: id.into(),
            label: "Invalid".into(),
            base_url: "http://localhost".into(),
            model: "test-model".into(),
            endpoint: "/chat/completions".into(),
            handler: "openai".into(),
            context_window_tokens: None,
            max_concurrency,
            reasoning_effort: String::new(),
            fast_mode,
            supports_prompt_cache_key: None,
            stream_usage: None,
            api_key: None,
        };

        let error = store.upsert_provider(update, before.revision).unwrap_err();
        assert!(error.1.contains(expected_error), "{}", error.1);
        let after = store.snapshot();
        assert_eq!(after.revision, before.revision);
        assert!(!after.providers.iter().any(|provider| provider.id == id));
    }

    let persisted = super::super::domains::load_providers().unwrap().unwrap();
    assert!(!persisted.providers.contains_key("zero-concurrency"));
    assert!(!persisted.providers.contains_key("non-codex-fast"));

    unsafe {
        match previous {
            Some(value) => std::env::set_var("BONE_DIR", value),
            None => std::env::remove_var("BONE_DIR"),
        }
    }
}

#[test]
fn provider_mutation_validates_and_normalizes_stream_usage() {
    let _guard = crate::util::test_env_lock();
    let previous = std::env::var_os("BONE_DIR");
    let dir = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("BONE_DIR", dir.path()) };

    let store = ConfigStore::new(crate::ext::ExtensionManager::unloaded()).unwrap();

    // A non-canonical stream_usage is rejected on the write path (matching the
    // load path), so it can never be persisted and break the next startup.
    let before = store.snapshot();
    let invalid = ProviderUpdate {
        id: "bad".into(),
        label: "Bad".into(),
        base_url: "http://localhost".into(),
        model: "test-model".into(),
        endpoint: "/chat/completions".into(),
        handler: "openai".into(),
        context_window_tokens: None,
        max_concurrency: None,
        reasoning_effort: String::new(),
        fast_mode: None,
        supports_prompt_cache_key: None,
        stream_usage: Some("maybe".into()),
        api_key: None,
    };
    let error = store.upsert_provider(invalid, before.revision).unwrap_err();
    assert!(
        error.1.contains("stream_usage must be"),
        "expected a stream_usage domain error, got: {error:?}"
    );
    let after = store.snapshot();
    assert_eq!(after.revision, before.revision);
    assert!(!after.providers.iter().any(|provider| provider.id == "bad"));

    // A case/whitespace variant is normalized to the canonical lowercase form,
    // exactly as the YAML load path does, and round-trips cleanly.
    let before = store.snapshot();
    let normalized = ProviderUpdate {
        id: "case".into(),
        label: "Case".into(),
        base_url: "http://localhost".into(),
        model: "test-model".into(),
        endpoint: "/chat/completions".into(),
        handler: "openai".into(),
        context_window_tokens: None,
        max_concurrency: None,
        reasoning_effort: String::new(),
        fast_mode: None,
        supports_prompt_cache_key: None,
        stream_usage: Some(" TRUE ".into()),
        api_key: None,
    };
    store.upsert_provider(normalized, before.revision).unwrap();
    let persisted = super::super::domains::load_providers().unwrap().unwrap();
    assert_eq!(persisted.providers["case"].stream_usage, "true");

    unsafe {
        match previous {
            Some(value) => std::env::set_var("BONE_DIR", value),
            None => std::env::remove_var("BONE_DIR"),
        }
    }
}

#[test]
fn command_policy_is_seeded_privately_and_malformed_content_fails_unchanged() {
    let _guard = crate::util::test_env_lock();
    let previous = std::env::var_os("BONE_DIR");
    let dir = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("BONE_DIR", dir.path()) };

    ConfigStore::new(crate::ext::ExtensionManager::unloaded()).unwrap();
    let path = dir.path().join("command-policy.yaml");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    let malformed = "read_only: [\n";
    std::fs::write(&path, malformed).unwrap();
    let error = ConfigStore::new(crate::ext::ExtensionManager::unloaded())
        .expect_err("malformed command policy should fail");
    assert!(error.contains("command-policy.yaml"));
    assert_eq!(std::fs::read_to_string(path).unwrap(), malformed);

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
        .expect_err("malformed configuration should fail");
    assert!(error.contains("cannot load"));

    unsafe {
        match previous {
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
        .set_path_at(
            "general.show_reasoning",
            serde_json::json!(true),
            &super::super::settings::settings_path(),
        )
        .unwrap();

    store.reload_settings().unwrap();

    let after = store.snapshot();
    assert_eq!(after.revision, before.revision + 1);
    assert_eq!(after.values["general"]["show_reasoning"], true);
    assert!(
        extensions
            .frontend_settings()
            .settings
            .general
            .show_reasoning
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
            max_concurrency: None,
            reasoning_effort: String::new(),
            fast_mode: false,
            supports_prompt_cache_key: false,
            stream_usage: "auto".into(),
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
fn successful_mutation_updates_every_runtime_sharing_the_canonical_handle() {
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
    assert!(Arc::ptr_eq(
        &initial.settings_handle(),
        &store.runtime_settings_handle()
    ));
    let attached = crate::ext::boot_with_tools_shared(
        &dir,
        &dir,
        &store,
        true,
        crate::ext::BootOptions::default(),
        "test-model",
        "test-provider",
        store.runtime_settings_handle(),
    )
    .manager;
    assert!(Arc::ptr_eq(
        &attached.settings_handle(),
        &store.runtime_settings_handle()
    ));
    let revision = store.snapshot().revision;
    store
        .set_value("general.show_reasoning", true.into(), revision)
        .unwrap();

    for extensions in [initial, attached] {
        assert!(
            extensions
                .frontend_settings()
                .settings
                .general
                .show_reasoning
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

#[test]
fn config_store_does_not_retain_the_constructor_lua_vm() {
    let manager = crate::ext::ExtensionManager::unloaded();
    let lua = manager.lua_arc();
    let weak = Arc::downgrade(&lua);
    drop(lua);

    let _store = ConfigStore::for_test_with_extensions(manager);
    assert!(
        weak.upgrade().is_none(),
        "configuration authority must retain only settings and catalog data"
    );
}

#[test]
fn extension_catalog_initialization_is_first_wins_and_reload_is_explicit() {
    use crate::config::settings::ExtensionValue;
    use crate::ext::settings_registry::{
        SettingsField, SettingsFieldType, SettingsPage, SettingsRegistry,
    };

    fn catalog(namespace: &str) -> SettingsRegistry {
        let mut registry = SettingsRegistry::default();
        registry
            .register(SettingsPage {
                namespace: namespace.into(),
                title: namespace.into(),
                owner: format!("{namespace}.lua"),
                command: None,
                fields: vec![SettingsField {
                    key: "enabled".into(),
                    label: "Enabled".into(),
                    field_type: SettingsFieldType::Bool,
                    options: Vec::new(),
                    default: ExtensionValue::Bool(true),
                    value: None,
                    integer: None,
                    min: None,
                    max: None,
                }],
            })
            .unwrap();
        registry
    }

    fn extension_names(store: &ConfigStore) -> Vec<String> {
        store
            .schema()
            .pages
            .into_iter()
            .find(|page| page.namespace == "extensions")
            .unwrap()
            .pages
            .into_iter()
            .map(|page| page.namespace)
            .collect()
    }

    let store = ConfigStore::for_test();
    assert!(store.initialize_extension_catalog(catalog("alpha")));
    assert!(!store.initialize_extension_catalog(catalog("attach_order_noise")));
    assert_eq!(extension_names(&store), ["alpha"]);

    store.replace_extension_catalog(catalog("beta"));
    assert_eq!(extension_names(&store), ["beta"]);
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
    assert!(persisted.subagents().is_empty());
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

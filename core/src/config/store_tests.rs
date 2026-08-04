use super::*;

#[test]
fn system_prompt_schema_mutation_reset_and_persistence() {
    let _guard = crate::util::test_env_lock();
    let previous = std::env::var_os("BONE_DIR");
    let dir = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("BONE_DIR", dir.path()) };

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
    assert_eq!(field.default, serde_json::Value::Null);

    let assert_value = |expected: Option<&str>| {
        let snapshot = store.snapshot();
        assert_eq!(
            snapshot.values["general"]["system_prompt"],
            expected
                .map(serde_json::Value::from)
                .unwrap_or(serde_json::Value::Null)
        );
        assert_eq!(
            store
                .runtime_settings_snapshot()
                .resolved()
                .general
                .system_prompt
                .as_deref(),
            expected
        );
        assert_eq!(
            Settings::load()
                .unwrap()
                .unwrap()
                .resolved()
                .general
                .system_prompt
                .as_deref(),
            expected
        );
        assert_eq!(
            std::fs::read_to_string(super::super::settings::settings_path())
                .unwrap()
                .contains("system_prompt:"),
            expected.is_some()
        );
    };

    store
        .set_value(
            "general.system_prompt",
            serde_json::json!("Configured base prompt"),
            store.snapshot().revision,
        )
        .unwrap();
    assert_value(Some("Configured base prompt"));

    store
        .set_value(
            "general.system_prompt",
            serde_json::Value::Null,
            store.snapshot().revision,
        )
        .unwrap();
    assert_value(None);

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
    assert_value(None);

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
        api_key: None,
    };

    store.upsert_provider(update, before.revision).unwrap();
    let after = store.snapshot();
    assert_eq!(after.revision, before.revision + 1);
    assert_eq!(
        after
            .providers
            .iter()
            .find(|provider| provider.id == "test")
            .unwrap()
            .reasoning_effort,
        "ultra"
    );

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
        .set_value("general", "show_thinking", "true".into())
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
fn successful_mutation_refreshes_attached_runtime_settings() {
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

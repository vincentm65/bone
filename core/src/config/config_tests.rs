use super::{
    DEFAULT_AGENTS_MD, DEFAULT_CORE_DOCS, InitChoice, ProviderEntry, ProvidersConfig,
    SetupSelection, api_key_required, apply_onboarding, domains, migrate_memory_to_catalog,
    migrate_memory_to_catalog_with_hash, needs_onboarding, seed_base, settings::SubagentSettings,
    sync_bundled_file,
};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

fn migration_test_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "bone-memory-catalog-{name}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ))
}

fn sha256(content: &[u8]) -> String {
    format!("{:x}", Sha256::digest(content))
}

fn with_test_bone_dir(test: impl FnOnce(&Path)) {
    let _guard = crate::util::test_env_lock();
    let dir = tempfile::tempdir().unwrap();
    let old_bone_dir = std::env::var_os("BONE_DIR");
    // SAFETY: held under test_env_lock; restored below.
    unsafe { std::env::set_var("BONE_DIR", dir.path()) };

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| test(dir.path())));

    match old_bone_dir {
        Some(value) => unsafe { std::env::set_var("BONE_DIR", value) },
        None => unsafe { std::env::remove_var("BONE_DIR") },
    }
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

fn empty_selection() -> SetupSelection {
    SetupSelection {
        plugins: Vec::new(),
    }
}

fn provider_entry(base_url: &str, handler: &str) -> ProviderEntry {
    ProviderEntry {
        label: "Test provider".into(),
        base_url: base_url.into(),
        model: "test-model".into(),
        api_key: Default::default(),
        endpoint: "/v1/chat/completions".into(),
        handler: handler.into(),
        context_window_tokens: None,
        request_timeout_s: None,
        max_concurrency: None,
        reasoning_effort: String::new(),
        fast_mode: false,
        supports_prompt_cache_key: false,
        stream_usage: "auto".into(),
    }
}

#[test]
fn api_key_required_classifies_local_and_cli_providers() {
    with_test_bone_dir(|_| {
        let cases = [
            ("local", "https://remote.example/v1", "openai", false),
            ("custom", "http://localhost:8080/v1", "openai", false),
            ("custom", "http://127.0.0.1:8080/v1", "openai", false),
            ("custom", "http://[::1]:8080/v1", "openai", false),
            ("remote", "https://remote.example/v1", "openai", true),
            ("bind", "http://0.0.0.0:8080/v1", "openai", true),
            ("claude", "https://remote.example", "claude_code", false),
        ];

        for (id, base_url, handler, required) in cases {
            let entry = provider_entry(base_url, handler);
            assert_eq!(
                api_key_required(id, &entry),
                required,
                "unexpected key requirement for {id} at {base_url}"
            );
        }
    });
}

#[test]
fn selected_keyless_provider_skips_onboarding_with_an_empty_key() {
    with_test_bone_dir(|_| {
        let mut config = ProvidersConfig::default();
        config.last_provider = "local".into();
        config.providers.insert(
            "local".into(),
            provider_entry("http://127.0.0.1:8080/v1", "openai"),
        );
        domains::persist_providers(&config).unwrap();

        assert!(config.providers["local"].api_key.is_empty());
        assert!(!needs_onboarding());
    });
}

#[test]
fn unselected_keyless_provider_does_not_suppress_onboarding() {
    with_test_bone_dir(|_| {
        let mut config = ProvidersConfig::default();
        config.providers.insert(
            "local".into(),
            provider_entry("http://127.0.0.1:8080/v1", "openai"),
        );
        domains::persist_providers(&config).unwrap();

        assert!(config.last_provider.is_empty());
        assert!(needs_onboarding());
    });
}

#[test]
fn seeded_default_providers_still_require_onboarding() {
    with_test_bone_dir(|_| {
        let config = domains::load_or_seed_providers().unwrap();

        assert!(config.last_provider.is_empty());
        assert!(config.providers["local"].api_key.is_empty());
        assert!(needs_onboarding());
    });
}

#[test]
fn populated_onboarding_writes_banner_and_canonical_researcher() {
    with_test_bone_dir(|dir| {
        apply_onboarding(&empty_selection(), InitChoice::Populated).unwrap();

        assert_eq!(
            fs::read_to_string(dir.join("init.lua")).unwrap(),
            "-- Bone init.lua\nrequire(\"banner\")\n"
        );
        let config = domains::load_subagents().unwrap().unwrap();
        assert_eq!(config.version, 1);
        assert_eq!(config.subagents.len(), 1);
        assert_eq!(
            config.subagents.get("researcher"),
            Some(&SubagentSettings {
                description:
                    "Investigates a question across the codebase and reports concise findings."
                        .into(),
                system_prompt: Some(
                    "You are a focused research agent. Investigate the assigned task thoroughly using the available tools, then report concrete findings with file:line references. Do not make edits."
                        .into(),
                ),
                ..Default::default()
            })
        );
    });
}

#[test]
fn populated_onboarding_preserves_existing_subagents() {
    with_test_bone_dir(|_| {
        let expected = BTreeMap::from([
            (
                "reviewer".into(),
                SubagentSettings {
                    description: "Reviews changes".into(),
                    system_prompt: Some("Review only".into()),
                    ..Default::default()
                },
            ),
            (
                "researcher".into(),
                SubagentSettings {
                    description: "My custom researcher".into(),
                    system_prompt: Some("Use my instructions".into()),
                    provider: Some("custom-provider".into()),
                    model: Some("custom-model".into()),
                    approval: "danger".into(),
                    timeout_ms: Some(42_000),
                    enabled: false,
                },
            ),
        ]);
        domains::persist_subagents(&expected).unwrap();

        apply_onboarding(&empty_selection(), InitChoice::Populated).unwrap();

        assert_eq!(
            domains::load_subagents().unwrap().unwrap().subagents,
            expected
        );
    });
}

#[test]
fn blank_and_keep_onboarding_do_not_modify_subagents() {
    with_test_bone_dir(|dir| {
        apply_onboarding(&empty_selection(), InitChoice::Blank).unwrap();
        assert!(!dir.join("subagents.yaml").exists());

        let subagents = "version: 1\nsubagents:\n  existing:\n    description: Existing agent\n";
        fs::write(dir.join("subagents.yaml"), subagents).unwrap();
        fs::write(dir.join("init.lua"), "-- existing init\n").unwrap();

        apply_onboarding(&empty_selection(), InitChoice::Keep).unwrap();

        assert_eq!(
            fs::read_to_string(dir.join("subagents.yaml")).unwrap(),
            subagents
        );
        assert_eq!(
            fs::read_to_string(dir.join("init.lua")).unwrap(),
            "-- existing init\n"
        );
    });
}

#[test]
fn bone_dir_prefers_bone_dir_env() {
    let _guard = crate::util::test_env_lock();

    let dir = std::env::temp_dir().join(format!(
        "bone-dir-env-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let old_bone = std::env::var_os("BONE_DIR");
    let old_xdg = std::env::var_os("XDG_CONFIG_HOME");
    // SAFETY: held under test_env_lock; restored below.
    unsafe {
        std::env::set_var("BONE_DIR", &dir);
        std::env::set_var("XDG_CONFIG_HOME", "/should/not/win");
    }
    let got = super::bone_dir();
    match old_bone {
        Some(v) => unsafe { std::env::set_var("BONE_DIR", v) },
        None => unsafe { std::env::remove_var("BONE_DIR") },
    }
    match old_xdg {
        Some(v) => unsafe { std::env::set_var("XDG_CONFIG_HOME", v) },
        None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
    }
    assert_eq!(got, dir);
}

#[test]
fn bone_dir_uses_xdg_when_bone_dir_unset() {
    let _guard = crate::util::test_env_lock();

    let xdg = std::env::temp_dir().join(format!(
        "bone-dir-xdg-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let old_bone = std::env::var_os("BONE_DIR");
    let old_xdg = std::env::var_os("XDG_CONFIG_HOME");
    unsafe {
        std::env::remove_var("BONE_DIR");
        std::env::set_var("XDG_CONFIG_HOME", &xdg);
    }
    let got = super::bone_dir();
    match old_bone {
        Some(v) => unsafe { std::env::set_var("BONE_DIR", v) },
        None => unsafe { std::env::remove_var("BONE_DIR") },
    }
    match old_xdg {
        Some(v) => unsafe { std::env::set_var("XDG_CONFIG_HOME", v) },
        None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
    }
    assert_eq!(got, xdg.join("bone-rust"));
}

#[test]
fn bundled_file_is_created_and_stale_content_is_replaced() {
    let dir = std::env::temp_dir().join(format!(
        "bone-sync-bundled-file-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let path = dir.join("AGENTS.md");
    fs::create_dir_all(&dir).unwrap();

    sync_bundled_file(&path, "version 1");
    assert_eq!(fs::read_to_string(&path).unwrap(), "version 1");

    fs::write(&path, "stale").unwrap();
    sync_bundled_file(&path, "version 2");
    assert_eq!(fs::read_to_string(&path).unwrap(), "version 2");

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn core_docs_are_synced_during_base_seed() {
    with_test_bone_dir(|dir| {
        seed_base().unwrap();

        for &(name, content) in DEFAULT_CORE_DOCS {
            assert_eq!(
                fs::read_to_string(dir.join("docs").join(name)).unwrap(),
                content
            );
        }

        for &(name, _) in DEFAULT_CORE_DOCS {
            fs::write(dir.join("docs").join(name), "stale").unwrap();
        }
        seed_base().unwrap();

        for &(name, content) in DEFAULT_CORE_DOCS {
            assert_eq!(
                fs::read_to_string(dir.join("docs").join(name)).unwrap(),
                content
            );
        }
    });
}

#[test]
fn fresh_seed_materializes_only_plugin_packages_under_lua() {
    with_test_bone_dir(|dir| {
        seed_base().unwrap();

        assert!(dir.join("lua/plugins/core/init.lua").is_file());
        assert!(dir.join("lua/plugins/core/lib/banner.lua").is_file());

        let mut seeded: Vec<_> = fs::read_dir(dir.join("lua"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        seeded.sort();
        assert_eq!(seeded, ["plugins"], "lua/ holds plugin packages only");
    });
}

#[test]
fn bundled_doc_index_only_references_synced_docs() {
    let synced = DEFAULT_CORE_DOCS
        .iter()
        .map(|(name, _)| *name)
        .collect::<std::collections::BTreeSet<_>>();
    let indexed = DEFAULT_AGENTS_MD
        .split('`')
        .filter_map(|value| value.strip_prefix("docs/"))
        .collect::<std::collections::BTreeSet<_>>();

    assert_eq!(indexed, synced);
}

#[test]
fn clean_memory_migration_marks_complete_before_catalog_install() {
    let dir = migration_test_dir("clean");
    fs::create_dir_all(&dir).unwrap();

    migrate_memory_to_catalog(&dir);
    assert!(dir.join(".memory-catalog-migrated").exists());

    let command = dir.join("lua/commands/memory.lua");
    fs::create_dir_all(command.parent().unwrap()).unwrap();
    fs::write(&command, "-- catalog command").unwrap();
    migrate_memory_to_catalog(&dir);
    assert_eq!(fs::read_to_string(&command).unwrap(), "-- catalog command");

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn known_bundled_memory_command_is_backed_up() {
    let dir = migration_test_dir("bundled");
    let command = dir.join("lua/commands/memory.lua");
    fs::create_dir_all(command.parent().unwrap()).unwrap();
    let bundled = b"-- bundled command";
    fs::write(&command, bundled).unwrap();

    migrate_memory_to_catalog_with_hash(&dir, &sha256(bundled));

    assert!(!command.exists());
    assert_eq!(
        fs::read(dir.join("lua/commands/memory.lua.bundled-backup")).unwrap(),
        bundled
    );
    assert!(dir.join(".memory-catalog-migrated").exists());

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn catalog_memory_command_present_before_migration_is_preserved() {
    let dir = migration_test_dir("catalog");
    let command = dir.join("lua/commands/memory.lua");
    fs::create_dir_all(command.parent().unwrap()).unwrap();
    fs::write(&command, "-- catalog command").unwrap();

    migrate_memory_to_catalog(&dir);

    assert_eq!(fs::read_to_string(&command).unwrap(), "-- catalog command");
    assert!(dir.join(".memory-catalog-migrated").exists());

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn user_modified_bundled_memory_command_is_preserved() {
    let dir = migration_test_dir("modified");
    let command = dir.join("lua/commands/memory.lua");
    fs::create_dir_all(command.parent().unwrap()).unwrap();
    let bundled = b"-- bundled command";
    let modified = b"-- bundled command\n-- user customization";
    fs::write(&command, modified).unwrap();

    migrate_memory_to_catalog_with_hash(&dir, &sha256(bundled));

    assert_eq!(fs::read(&command).unwrap(), modified);
    assert!(dir.join(".memory-catalog-migrated").exists());

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn memory_catalog_migration_does_not_overwrite_scoped_data() {
    let dir = migration_test_dir("scoped");
    fs::create_dir_all(dir.join("memory")).unwrap();
    fs::write(dir.join("memory.md"), "legacy memory").unwrap();
    fs::write(dir.join("memory/global.md"), "scoped memory").unwrap();

    migrate_memory_to_catalog(&dir);

    assert_eq!(
        fs::read_to_string(dir.join("memory/global.md")).unwrap(),
        "scoped memory"
    );
    assert!(dir.join(".memory-catalog-migrated").exists());

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn failed_memory_command_backup_leaves_migration_unmarked() {
    let dir = migration_test_dir("failed-backup");
    let command = dir.join("lua/commands/memory.lua");
    fs::create_dir_all(command.parent().unwrap()).unwrap();
    let bundled = b"-- bundled command";
    fs::write(&command, bundled).unwrap();
    fs::create_dir(dir.join("lua/commands/memory.lua.bundled-backup")).unwrap();

    migrate_memory_to_catalog_with_hash(&dir, &sha256(bundled));

    assert!(command.exists());
    assert!(!dir.join(".memory-catalog-migrated").exists());

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn memory_catalog_migration_copies_legacy_data() {
    let dir = migration_test_dir("legacy-data");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("memory.md"), "legacy memory").unwrap();

    migrate_memory_to_catalog(&dir);

    assert_eq!(
        fs::read_to_string(dir.join("memory/global.md")).unwrap(),
        "legacy memory"
    );
    assert!(dir.join(".memory-catalog-migrated").exists());

    fs::remove_dir_all(dir).unwrap();
}

use super::{
    DEFAULT_AGENTS_MD, DEFAULT_CORE_DOCS, InitChoice, ProviderEntry, ProvidersConfig,
    SetupSelection, api_key_required, apply_onboarding, domains, needs_onboarding, seed_base,
    settings::SubagentSettings, sync_bundled_file,
};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

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
fn fresh_seed_materializes_core_and_plugin_packages_under_lua() {
    with_test_bone_dir(|dir| {
        seed_base().unwrap();

        assert!(dir.join("lua/core/init.lua").is_file());
        assert!(dir.join("lua/core/lib/banner.lua").is_file());
        assert!(
            !dir.join("lua/plugins/core").exists(),
            "core must not be seeded under lua/plugins"
        );

        let mut seeded: Vec<_> = fs::read_dir(dir.join("lua"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        seeded.sort();
        assert_eq!(
            seeded,
            ["core", "plugins"],
            "lua/ holds the built-in core and optional plugin packages"
        );
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

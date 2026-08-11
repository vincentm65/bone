use super::*;
use std::fs;
use std::path::Path;

fn with_host_env(test: impl FnOnce(&Path, &Path, ConfigStore)) {
    let _guard = crate::util::test_env_lock();
    let bone = tempfile::tempdir().unwrap();
    let fixture = tempfile::tempdir().unwrap();
    fs::write(fixture.path().join("catalog.json"), "[]").unwrap();
    let old_bone = std::env::var_os("BONE_DIR");
    let old_catalog = std::env::var_os("BONE_CATALOG_URL");
    // SAFETY: environment mutation is serialized by the process-wide test lock
    // and both values are restored before releasing it.
    unsafe {
        std::env::set_var("BONE_DIR", bone.path());
        std::env::set_var("BONE_CATALOG_URL", fixture.path());
    }
    let config = ConfigStore::new(crate::ext::ExtensionManager::unloaded()).unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        test(bone.path(), fixture.path(), config)
    }));
    unsafe {
        match old_bone {
            Some(value) => std::env::set_var("BONE_DIR", value),
            None => std::env::remove_var("BONE_DIR"),
        }
        match old_catalog {
            Some(value) => std::env::set_var("BONE_CATALOG_URL", value),
            None => std::env::remove_var("BONE_CATALOG_URL"),
        }
    }
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

#[test]
fn catalog_projection_is_display_safe_and_revisioned() {
    with_host_env(|_, _, _| {
        let entry = CatalogEntry {
            name: "weather.lua".into(),
            kind: "tool".into(),
            description: "Weather".into(),
            sha256: "secret-integrity-detail".into(),
            files: vec![crate::ext::catalog::CatalogFile {
                path: "lib/helper.lua".into(),
                sha256: "also-internal".into(),
            }],
            ..CatalogEntry::default()
        };
        let first = catalog_snapshot(std::slice::from_ref(&entry));
        let second = catalog_snapshot(&[entry]);
        assert_eq!(first.revision, second.revision);
        assert_eq!(first.items[0].name, "weather.lua");
        let json = serde_json::to_value(&first.items[0]).unwrap();
        assert!(json.get("sha256").is_none());
        assert!(json.get("files").is_none());
    });
}

#[test]
fn stats_queries_the_service_database() {
    with_host_env(|bone, _, config| {
        let path = bone.join("stats.db");
        let db = SessionDb::open(&path).unwrap();
        let id = db.create_conversation("openai", "gpt").unwrap();
        db.record_usage(id, "openai", "gpt", 100, 25, Some(10), Some(0.5), false)
            .unwrap();
        let service = HostService::with_db_path(config, path);
        let HostResponse::Stats(snapshot) = service.execute(HostRequest::Stats { range: None })
        else {
            panic!("expected stats response");
        };
        assert_eq!(snapshot.total.prompt_tokens, 100);
        assert_eq!(snapshot.total.completion_tokens, 25);
        assert_eq!(snapshot.total.cached_tokens, 10);
        assert_eq!(snapshot.total.request_count, 1);
    });
}

#[test]
fn catalog_apply_installs_and_removes_with_per_item_results() {
    with_host_env(|bone, fixture, config| {
        let content = b"return { weather = true }\n";
        fs::create_dir_all(fixture.join("tools")).unwrap();
        fs::write(fixture.join("tools/weather.lua"), content).unwrap();
        let sha256 = format!("{:x}", Sha256::digest(content));
        fs::write(
            fixture.join("catalog.json"),
            serde_json::to_vec(&serde_json::json!([{
                "name": "weather.lua",
                "kind": "tool",
                "description": "Weather",
                "sha256": sha256
            }]))
            .unwrap(),
        )
        .unwrap();

        let service = HostService::new(config);
        let HostResponse::Catalog(snapshot) =
            service.execute(HostRequest::Catalog { refresh: true })
        else {
            panic!("expected catalog response");
        };
        let install = HostRequest::CatalogApply {
            expected_revision: snapshot.revision.clone(),
            actions: vec![CatalogAction {
                name: "weather".into(),
                action: CatalogActionKind::Install,
            }],
        };
        let HostResponse::CatalogApplied(result) = service.execute(install) else {
            panic!("expected catalog apply response");
        };
        assert!(result.changed);
        assert!(!result.extensions_reloaded);
        assert!(matches!(
            result.results[0].outcome,
            CatalogItemOutcome::Installed
        ));
        assert_ne!(result.snapshot.revision, snapshot.revision);
        assert!(bone.join("lua/tools/weather.lua").exists());

        let remove = HostRequest::CatalogApply {
            expected_revision: result.snapshot.revision,
            actions: vec![CatalogAction {
                name: "weather.lua".into(),
                action: CatalogActionKind::Remove,
            }],
        };
        let HostResponse::CatalogApplied(result) = service.execute(remove) else {
            panic!("expected catalog remove response");
        };
        assert!(matches!(
            result.results[0].outcome,
            CatalogItemOutcome::Removed
        ));
        assert!(!bone.join("lua/tools/weather.lua").exists());
    });
}

#[test]
fn setup_apply_checks_revisions_then_uses_existing_onboarding_path() {
    with_host_env(|bone, _, config| {
        let service = HostService::new(config);
        let HostResponse::Setup(snapshot) = service.execute(HostRequest::Setup) else {
            panic!("expected setup response");
        };
        assert!(matches!(
            service.execute(HostRequest::SetupApply {
                expected_config_revision: snapshot.config_revision,
                expected_catalog_revision: "stale".into(),
                provider_id: None,
                api_key: None,
                catalog: vec![],
                init: InitChoice::Blank,
            }),
            HostResponse::Error {
                code: HostErrorCode::Stale,
                ..
            }
        ));
        assert!(!bone.join("init.lua").exists());

        let HostResponse::SetupApplied(result) = service.clone().execute(HostRequest::SetupApply {
            expected_config_revision: snapshot.config_revision,
            expected_catalog_revision: snapshot.catalog.revision,
            provider_id: None,
            api_key: None,
            catalog: vec![],
            init: InitChoice::Blank,
        }) else {
            panic!("expected setup apply response");
        };
        assert!(result.restart_required);
        assert!(!result.catalog.extensions_reloaded);
        assert!(bone.join("init.lua").exists());
        assert!(bone.join(".setup.json").exists());
    });
}

#[test]
fn populated_setup_refreshes_config_store_before_followup_mutation() {
    with_host_env(|_, _, config| {
        let service = HostService::new(config.clone());
        let HostResponse::Setup(snapshot) = service.execute(HostRequest::Setup) else {
            panic!("expected setup response");
        };
        let initial_revision = snapshot.config_revision;

        let HostResponse::SetupApplied(result) = service.execute(HostRequest::SetupApply {
            expected_config_revision: initial_revision,
            expected_catalog_revision: snapshot.catalog.revision,
            provider_id: None,
            api_key: None,
            catalog: vec![],
            init: InitChoice::Populated,
        }) else {
            panic!("expected setup apply response");
        };
        assert_eq!(result.config_revision, initial_revision + 1);
        assert!(config.snapshot().values["subagents"]["researcher"].is_object());

        config
            .upsert_subagent(
                bone_protocol::SubagentDefinition {
                    name: "reviewer".into(),
                    description: "Reviews changes".into(),
                    approval: "safe".into(),
                    enabled: true,
                    source: "config".into(),
                    ..Default::default()
                },
                result.config_revision,
            )
            .unwrap();
        let persisted = crate::config::domains::load_subagents()
            .unwrap()
            .unwrap()
            .subagents;
        assert!(persisted.contains_key("researcher"));
        assert!(persisted.contains_key("reviewer"));
    });
}

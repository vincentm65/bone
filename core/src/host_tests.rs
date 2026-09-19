use super::*;
use crate::llm::{ChatMessage, ChatRole};
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
        let first = catalog_snapshot(std::slice::from_ref(&entry), &[]);
        let second = catalog_snapshot(&[entry], &[]);
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
fn conversations_lists_recent_metadata_through_the_service() {
    with_host_env(|bone, _, config| {
        let path = bone.join("conversations.db");
        let db = SessionDb::open(&path).unwrap();
        let chat = db.create_conversation("openai", "gpt").unwrap();
        let mut message = ChatMessage::new(ChatRole::User, "hello world");
        message.created_at = Some("2026-07-03T08:00:00Z".into());
        db.append_chat_message(chat, &message, 1).unwrap();
        let empty = db.create_conversation("anthropic", "claude").unwrap();
        db.conn_ref()
            .execute(
                "UPDATE conversations SET started_at = '2026-07-01T00:00:00Z' WHERE id = ?1",
                [empty],
            )
            .unwrap();
        drop(db);

        let service = HostService::with_db_path(config, path);

        // A zero limit selects the daemon default and returns both rows.
        let HostResponse::Conversations(conversations) =
            service.execute(HostRequest::Conversations { limit: 0 })
        else {
            panic!("expected conversations response");
        };
        assert_eq!(conversations.len(), 2);
        assert_eq!(conversations[0].id, chat);
        assert_eq!(conversations[0].title, "hello world");
        assert_eq!(conversations[1].id, empty);
        assert_eq!(conversations[1].title, "(new)");

        // An explicit limit keeps only the most recent conversation.
        let HostResponse::Conversations(limited) =
            service.execute(HostRequest::Conversations { limit: 1 })
        else {
            panic!("expected conversations response");
        };
        assert_eq!(limited.iter().map(|c| c.id).collect::<Vec<_>>(), vec![chat]);
    });
}

#[test]
fn conversation_mutation_responses_honor_the_requested_limit() {
    with_host_env(|bone, _, config| {
        let path = bone.join("conversations.db");
        let db = SessionDb::open(&path).unwrap();
        let first = db.create_conversation("openai", "gpt").unwrap();
        let mut message = ChatMessage::new(ChatRole::User, "first chat");
        message.created_at = Some("2026-07-03T08:00:00Z".into());
        db.append_chat_message(first, &message, 1).unwrap();
        let second = db.create_conversation("anthropic", "claude").unwrap();
        db.conn_ref()
            .execute(
                "UPDATE conversations SET started_at = '2026-07-01T00:00:00Z' WHERE id = ?1",
                [second],
            )
            .unwrap();
        drop(db);
        let service = HostService::with_db_path(config, path);

        // A rename refresh is capped to the requested limit.
        let HostResponse::Conversations(renamed) =
            service.execute(HostRequest::ConversationRename {
                id: first,
                title: "renamed".into(),
                limit: 1,
            })
        else {
            panic!("expected conversations response");
        };
        assert_eq!(
            renamed.iter().map(|c| c.id).collect::<Vec<_>>(),
            vec![first]
        );

        // A delete refresh threads the same limit through the resolver.
        let HostResponse::Conversations(deleted) =
            service.execute(HostRequest::ConversationDelete {
                id: second,
                limit: 1,
            })
        else {
            panic!("expected conversations response");
        };
        assert_eq!(
            deleted.iter().map(|c| c.id).collect::<Vec<_>>(),
            vec![first]
        );

        // An oversized limit is capped rather than rejected.
        let HostResponse::Conversations(capped) =
            service.execute(HostRequest::Conversations { limit: u32::MAX })
        else {
            panic!("expected conversations response");
        };
        assert_eq!(capped.iter().map(|c| c.id).collect::<Vec<_>>(), vec![first]);
    });
}

#[test]
fn conversations_reports_unavailable_when_the_database_cannot_be_opened() {
    with_host_env(|bone, _, config| {
        let blocker = bone.join("blocker");
        fs::write(&blocker, b"a file where the database directory would go").unwrap();
        let service = HostService::with_db_path(config, blocker.join("conversations.db"));
        let response = service.execute(HostRequest::Conversations { limit: 10 });
        assert!(
            matches!(
                response,
                HostResponse::Error {
                    code: HostErrorCode::Unavailable,
                    ..
                }
            ),
            "expected unavailable error, got {response:?}"
        );
    });
}

#[test]
fn conversation_rename_persists_the_title_and_refreshes_the_list() {
    with_host_env(|bone, _, config| {
        let path = bone.join("conversations.db");
        let db = SessionDb::open(&path).unwrap();
        let chat = db.create_conversation("openai", "gpt").unwrap();
        let mut message = ChatMessage::new(ChatRole::User, "hello world");
        message.created_at = Some("2026-07-03T08:00:00Z".into());
        db.append_chat_message(chat, &message, 1).unwrap();
        drop(db);
        let service = HostService::with_db_path(config, path);

        let HostResponse::Conversations(conversations) =
            service.execute(HostRequest::ConversationRename {
                id: chat,
                title: "  debugging notes  ".into(),
                limit: 0,
            })
        else {
            panic!("expected conversations response");
        };
        assert_eq!(conversations.len(), 1);
        assert_eq!(conversations[0].id, chat);
        assert_eq!(conversations[0].title, "debugging notes");

        // A blank rename is rejected: the desktop refuses empty titles
        // client-side and the host backstops the same rule.
        let blank = service.execute(HostRequest::ConversationRename {
            id: chat,
            title: "   ".into(),
            limit: 0,
        });
        assert!(
            matches!(
                blank,
                HostResponse::Error {
                    code: HostErrorCode::Invalid,
                    ..
                }
            ),
            "expected invalid error for a blank rename, got {blank:?}"
        );
    });
}

#[test]
fn conversation_rename_rejects_empty_titles_and_unknown_ids() {
    with_host_env(|bone, _, config| {
        let path = bone.join("conversations.db");
        let db = SessionDb::open(&path).unwrap();
        db.create_conversation("openai", "gpt").unwrap();
        drop(db);
        let service = HostService::with_db_path(config, path);

        let empty = service.execute(HostRequest::ConversationRename {
            id: 1,
            title: "   ".into(),
            limit: 0,
        });
        assert!(
            matches!(
                empty,
                HostResponse::Error {
                    code: HostErrorCode::Invalid,
                    ..
                }
            ),
            "expected invalid error, got {empty:?}"
        );

        let unknown = service.execute(HostRequest::ConversationRename {
            id: 999,
            title: "no such chat".into(),
            limit: 0,
        });
        assert!(
            matches!(
                unknown,
                HostResponse::Error {
                    code: HostErrorCode::Invalid,
                    ..
                }
            ),
            "expected invalid error, got {unknown:?}"
        );
    });
}

#[test]
fn conversation_delete_removes_the_row_and_refreshes_the_list() {
    with_host_env(|bone, _, config| {
        let path = bone.join("conversations.db");
        let db = SessionDb::open(&path).unwrap();
        let chat = db.create_conversation("openai", "gpt").unwrap();
        let mut message = ChatMessage::new(ChatRole::User, "hello world");
        message.created_at = Some("2026-07-03T08:00:00Z".into());
        db.append_chat_message(chat, &message, 1).unwrap();
        db.record_usage(chat, "openai", "gpt", 10, 5, None, Some(0.1), false)
            .unwrap();
        let other = db.create_conversation("anthropic", "claude").unwrap();
        drop(db);
        let service = HostService::with_db_path(config, path);

        let HostResponse::Conversations(conversations) =
            service.execute(HostRequest::ConversationDelete { id: chat, limit: 0 })
        else {
            panic!("expected conversations response");
        };
        assert_eq!(
            conversations.iter().map(|c| c.id).collect::<Vec<_>>(),
            vec![other]
        );

        // Deleting the same id again is an invalid request.
        let again = service.execute(HostRequest::ConversationDelete { id: chat, limit: 0 });
        assert!(
            matches!(
                again,
                HostResponse::Error {
                    code: HostErrorCode::Invalid,
                    ..
                }
            ),
            "expected invalid error, got {again:?}"
        );
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
        assert!(bone.join("lua/plugins/weather/init.lua").exists());

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
        assert!(!bone.join("lua/plugins/weather").exists());
    });
}

#[test]
fn catalog_apply_enables_and_disables_plugins_only() {
    with_host_env(|_, fixture, config| {
        fs::create_dir_all(fixture.join("plugins/alpha")).unwrap();
        fs::write(fixture.join("plugins/alpha/init.lua"), b"return {}\n").unwrap();
        fs::create_dir_all(fixture.join("tools")).unwrap();
        fs::write(fixture.join("tools/weather.lua"), b"return {}\n").unwrap();
        fs::write(
            fixture.join("catalog.json"),
            serde_json::to_vec(&serde_json::json!([
                { "name": "alpha", "kind": "plugin", "description": "Alpha plugin" },
                { "name": "weather.lua", "kind": "tool", "description": "Weather" }
            ]))
            .unwrap(),
        )
        .unwrap();

        let service = HostService::new(config.clone());
        let HostResponse::Catalog(snapshot) =
            service.execute(HostRequest::Catalog { refresh: true })
        else {
            panic!("expected catalog response");
        };
        let item = |snapshot: &CatalogSnapshot, name: &str| {
            snapshot
                .items
                .iter()
                .find(|item| item.name == name)
                .unwrap()
                .clone()
        };
        assert!(item(&snapshot, "alpha").enabled, "plugins start enabled");
        assert!(
            item(&snapshot, "weather.lua").enabled,
            "non-plugins always enabled"
        );

        let apply = |expected: String, name: &str, action: CatalogActionKind| {
            let HostResponse::CatalogApplied(result) = service.execute(HostRequest::CatalogApply {
                expected_revision: expected,
                actions: vec![CatalogAction {
                    name: name.into(),
                    action,
                }],
            }) else {
                panic!("expected catalog apply response");
            };
            result
        };

        let result = apply(
            snapshot.revision.clone(),
            "alpha",
            CatalogActionKind::Disable,
        );
        assert!(
            matches!(result.results[0].outcome, CatalogItemOutcome::Disabled),
            "got {:?}",
            result.results[0].outcome
        );
        assert!(result.changed);
        assert_eq!(config.disabled_plugins(), vec!["alpha".to_string()]);
        assert!(!item(&result.snapshot, "alpha").enabled);
        assert_ne!(result.snapshot.revision, snapshot.revision);

        // Re-disabling is idempotent and does not change state.
        let result = apply(
            result.snapshot.revision.clone(),
            "alpha",
            CatalogActionKind::Disable,
        );
        assert!(matches!(
            result.results[0].outcome,
            CatalogItemOutcome::Unchanged
        ));
        assert!(!result.changed);

        let result = apply(
            result.snapshot.revision.clone(),
            "alpha",
            CatalogActionKind::Enable,
        );
        assert!(matches!(
            result.results[0].outcome,
            CatalogItemOutcome::Enabled
        ));
        assert!(config.disabled_plugins().is_empty());
        assert!(item(&result.snapshot, "alpha").enabled);

        // Non-plugin capabilities have no package-level enable state.
        let result = apply(
            result.snapshot.revision.clone(),
            "weather.lua",
            CatalogActionKind::Disable,
        );
        assert!(matches!(
            result.results[0].outcome,
            CatalogItemOutcome::Failed { .. }
        ));
        assert!(!result.changed);
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

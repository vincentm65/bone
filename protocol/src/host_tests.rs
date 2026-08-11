use super::*;

fn stats() -> UsageStatsSnapshot {
    let bucket = UsageBucket {
        label: "2026-08-10".into(),
        prompt_tokens: 10,
        completion_tokens: 3,
        cached_tokens: 2,
        cost: 0.25,
        request_count: 1,
    };
    let hourly = HourUsage {
        hour: 14,
        prompt_tokens: 10,
        completion_tokens: 3,
        cached_tokens: 2,
        request_count: 1,
    };
    let provider = ProviderUsage {
        provider: "openai".into(),
        model: "gpt".into(),
        prompt_tokens: 10,
        completion_tokens: 3,
        cached_tokens: 2,
        cost: 0.25,
        request_count: 1,
    };
    UsageStatsSnapshot {
        started_at: Some("2026-08-10 14:00:00".into()),
        ended_at: Some("2026-08-10 14:01:00".into()),
        total: UsageSummary {
            prompt_tokens: 10,
            completion_tokens: 3,
            cached_tokens: 2,
            cost: 0.25,
            request_count: 1,
        },
        by_model_today: vec![provider.clone()],
        by_model_7d: vec![provider.clone()],
        by_model_4w: vec![provider.clone()],
        by_model_all: vec![provider],
        daily: vec![bucket.clone()],
        weekly: vec![bucket.clone()],
        monthly: vec![bucket.clone()],
        all_time: vec![bucket.clone()],
        yearly: vec![bucket.clone()],
        hourly_today: vec![hourly.clone()],
        hourly_7d: vec![hourly.clone()],
        hourly_4w: vec![hourly.clone()],
        hourly_all: vec![hourly],
        daily_activity: vec![bucket],
    }
}

fn catalog() -> CatalogSnapshot {
    CatalogSnapshot {
        revision: "sha256:index".into(),
        items: vec![CatalogItem {
            name: "weather.lua".into(),
            kind: "tool".into(),
            description: "Weather lookup".into(),
            version: Some("1.2.0".into()),
            updated_at: Some("2026-08-10".into()),
            author: Some("Bone".into()),
            repository: Some("https://example.test/repo".into()),
            documentation: Some("https://example.test/docs".into()),
            min_bone_version: Some("0.9".into()),
            dependencies: vec!["curl".into()],
            permissions: vec!["network".into()],
            long_description: Some("Detailed weather forecasts.".into()),
            installed: true,
            update_available: true,
        }],
    }
}

fn action() -> CatalogAction {
    CatalogAction {
        name: "weather.lua".into(),
        action: CatalogActionKind::Install,
    }
}

fn applied() -> CatalogApplyResult {
    CatalogApplyResult {
        snapshot: catalog(),
        results: vec![CatalogItemResult {
            name: "weather.lua".into(),
            outcome: CatalogItemOutcome::Installed,
        }],
        changed: true,
        extensions_reloaded: true,
    }
}

fn setup() -> SetupSnapshot {
    SetupSnapshot {
        config_revision: 7,
        providers: vec![ProviderChoice {
            id: "openai".into(),
            label: "OpenAI".into(),
            api_key_configured: true,
        }],
        active_provider: "openai".into(),
        init_exists: true,
        needs_onboarding: false,
        catalog: catalog(),
    }
}

fn roundtrip<T>(value: &T) -> T
where
    T: Serialize + for<'de> Deserialize<'de>,
{
    serde_json::from_str(&serde_json::to_string(value).unwrap()).unwrap()
}

#[test]
fn every_host_request_variant_round_trips() {
    let variants = vec![
        HostRequest::Stats { range: None },
        HostRequest::Stats {
            range: Some(DateRange {
                start: "2026-08-01".into(),
                end: "2026-08-10".into(),
            }),
        },
        HostRequest::Catalog { refresh: true },
        HostRequest::CatalogApply {
            expected_revision: "sha256:index".into(),
            actions: vec![action()],
        },
        HostRequest::Setup,
        HostRequest::SetupApply {
            expected_config_revision: 7,
            expected_catalog_revision: "sha256:index".into(),
            provider_id: Some("openai".into()),
            api_key: Some("secret".into()),
            catalog: vec![action()],
            init: InitChoice::Populated,
        },
    ];
    for value in variants {
        assert_eq!(roundtrip(&value), value);
    }
}

#[test]
fn every_host_response_variant_round_trips() {
    let variants = vec![
        HostResponse::Stats(Box::new(stats())),
        HostResponse::Catalog(catalog()),
        HostResponse::CatalogApplied(applied()),
        HostResponse::Setup(setup()),
        HostResponse::SetupApplied(SetupApplyResult {
            config_revision: 9,
            catalog: applied(),
            restart_required: true,
            message: "Setup saved.".into(),
        }),
        HostResponse::Error {
            code: HostErrorCode::Stale,
            message: "catalog changed".into(),
        },
    ];
    for value in variants {
        assert_eq!(roundtrip(&value), value);
    }
}

#[test]
fn request_defaults_preserve_snapshot_only_and_cached_catalog_behavior() {
    let stats: HostRequest = serde_json::from_str(r#"{"stats":{}}"#).unwrap();
    assert_eq!(stats, HostRequest::Stats { range: None });

    let catalog: HostRequest = serde_json::from_str(r#"{"catalog":{}}"#).unwrap();
    assert_eq!(catalog, HostRequest::Catalog { refresh: false });

    let item: CatalogItem =
        serde_json::from_str(r#"{"name":"weather.lua","kind":"tool"}"#).unwrap();
    assert_eq!(
        item,
        CatalogItem {
            name: "weather.lua".into(),
            kind: "tool".into(),
            ..CatalogItem::default()
        }
    );
}

#[test]
fn usage_snapshot_keeps_frontend_view_selection_local() {
    let snapshot = stats();
    assert_eq!(snapshot.buckets(0usize), snapshot.daily);
    assert_eq!(snapshot.buckets(4usize), snapshot.all_time);
    assert_eq!(snapshot.hourly(3usize), snapshot.hourly_all);
    assert_eq!(snapshot.range_summary(0usize), snapshot.total);
    assert_eq!(snapshot.range_models(1usize), snapshot.by_model_7d);
}

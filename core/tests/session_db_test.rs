use bone_core::session_db::SessionDb;
use std::sync::atomic::{AtomicU64, Ordering};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn test_db() -> SessionDb {
    let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "bone_session_db_test_{}_{}",
        std::process::id(),
        id
    ));
    SessionDb::open(&path).unwrap()
}

#[test]
fn create_and_end_conversation() {
    let db = test_db();
    let id = db.create_conversation("test", "model-1").unwrap();
    assert!(id > 0);
    db.end_conversation(id).unwrap();

    let usage = db.usage_by_provider(id).unwrap();
    assert!(usage.is_empty());
}

#[test]
fn record_and_sum_usage() {
    let db = test_db();
    let conv_id = db.create_conversation("test", "m").unwrap();
    db.record_usage(conv_id, "test", "m", 100, 50, Some(20), Some(0.01), false)
        .unwrap();
    db.record_usage(conv_id, "test", "m", 200, 80, None, None, false)
        .unwrap();

    let by_provider = db.usage_by_provider(conv_id).unwrap();
    assert_eq!(by_provider.len(), 1);
    let usage = &by_provider[0];
    assert_eq!(usage.prompt_tokens, 300);
    assert_eq!(usage.completion_tokens, 130);
    assert_eq!(usage.cached_tokens, 20);
    assert!((usage.cost - 0.01).abs() < f64::EPSILON);
    assert_eq!(usage.request_count, 2);
}

#[test]
fn usage_by_provider_grouping() {
    let db = test_db();
    let conv_id = db.create_conversation("test", "m").unwrap();
    db.record_usage(
        conv_id,
        "glm",
        "glm-4",
        1000,
        100,
        Some(500),
        Some(0.1),
        false,
    )
    .unwrap();
    db.record_usage(
        conv_id,
        "openrouter",
        "claude-3",
        2000,
        200,
        None,
        Some(0.2),
        false,
    )
    .unwrap();
    db.record_usage(conv_id, "glm", "glm-4", 500, 50, None, None, false)
        .unwrap();

    let by_provider = db.usage_by_provider(conv_id).unwrap();
    assert_eq!(by_provider.len(), 2);

    let glm = by_provider.iter().find(|p| p.provider == "glm").unwrap();
    assert_eq!(glm.prompt_tokens, 1500);
    assert_eq!(glm.completion_tokens, 150);
    assert_eq!(glm.cached_tokens, 500);
    assert!((glm.cost - 0.1).abs() < f64::EPSILON);
    assert_eq!(glm.request_count, 2);

    let or = by_provider
        .iter()
        .find(|p| p.provider == "openrouter")
        .unwrap();
    assert_eq!(or.prompt_tokens, 2000);
    assert_eq!(or.request_count, 1);
}

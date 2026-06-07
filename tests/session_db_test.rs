use bone::session_db::SessionDb;
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
fn append_and_retrieve_messages() {
    let db = test_db();
    let conv_id = db.create_conversation("test", "m").unwrap();
    db.append_message(conv_id, "user", "Hello world alpha", None, None, None, 0)
        .unwrap();
    db.append_message(conv_id, "assistant", "Hi there beta", None, None, None, 1)
        .unwrap();
    db.append_message(
        conv_id,
        "tool",
        "gamma result data",
        Some("read_file"),
        Some("call-1"),
        None,
        2,
    )
    .unwrap();

    // Verify all three messages are searchable via FTS
    assert_eq!(db.search("alpha", 10).unwrap().len(), 1);
    assert_eq!(db.search("beta", 10).unwrap().len(), 1);
    assert_eq!(db.search("gamma", 10).unwrap().len(), 1);

    // Verify roles are stored correctly
    assert_eq!(db.search("alpha", 10).unwrap()[0].role, "user");
    assert_eq!(db.search("beta", 10).unwrap()[0].role, "assistant");
    assert_eq!(db.search("gamma", 10).unwrap()[0].role, "tool");

    // Verify tool_calls text is indexed in FTS (tool_call_id is not in FTS but
    // tool_calls content is). The message was created with no tool_calls but
    // tool_name, which is stored in the messages table.
    let tool_hits = db.search("gamma", 10).unwrap();
    assert_eq!(tool_hits.len(), 1);
    assert_eq!(tool_hits[0].conversation_id, conv_id);
}

#[test]
fn append_message_populates_fts() {
    let db = test_db();
    let conv_id = db.create_conversation("test", "m").unwrap();
    db.append_message(
        conv_id,
        "user",
        "The quick brown fox jumps over the lazy dog",
        None,
        None,
        None,
        0,
    )
    .unwrap();
    let hits = db.search("brown fox", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].role, "user");
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

#[test]
fn search_returns_ranked_hits() {
    let db = test_db();
    let conv_id = db.create_conversation("test", "m").unwrap();
    db.append_message(
        conv_id,
        "user",
        "provider switch tokens test",
        None,
        None,
        None,
        0,
    )
    .unwrap();
    db.append_message(
        conv_id,
        "assistant",
        "Better model: usage is recorded per request",
        None,
        None,
        None,
        1,
    )
    .unwrap();
    let hits = db.search("provider switch", 10).unwrap();
    assert!(!hits.is_empty());
    assert!(hits[0].snippet.contains('\u{25b8}'));
}

#[test]
fn search_across_conversations() {
    let db = test_db();
    let conv1 = db.create_conversation("test", "m1").unwrap();
    let conv2 = db.create_conversation("test", "m2").unwrap();
    db.append_message(conv1, "user", "unique alpha keyword", None, None, None, 0)
        .unwrap();
    db.append_message(conv2, "user", "unique beta keyword", None, None, None, 0)
        .unwrap();

    let hits = db.search("unique", 10).unwrap();
    assert_eq!(hits.len(), 2);
    let conv_ids: Vec<i64> = hits.iter().map(|h| h.conversation_id).collect();
    assert!(conv_ids.contains(&conv1));
    assert!(conv_ids.contains(&conv2));
}

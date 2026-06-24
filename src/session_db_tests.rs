use super::{SessionDb, civil_from_days, iso_from_unix_secs};
use rusqlite::Connection;

/// The original v1 schema, before `is_estimated` and the created_at index.
const V1_SCHEMA: &str = "
    CREATE TABLE conversations (
        id INTEGER PRIMARY KEY, started_at TEXT NOT NULL, ended_at TEXT,
        provider TEXT NOT NULL, model TEXT NOT NULL
    );
    CREATE TABLE messages (
        id INTEGER PRIMARY KEY, conversation_id INTEGER NOT NULL,
        role TEXT NOT NULL, content TEXT NOT NULL, tool_name TEXT,
        tool_call_id TEXT, seq INTEGER NOT NULL,
        created_at TEXT NOT NULL
    );
    CREATE TABLE usage_events (
        id INTEGER PRIMARY KEY, conversation_id INTEGER NOT NULL,
        provider TEXT NOT NULL, model TEXT NOT NULL,
        prompt_tokens INTEGER NOT NULL DEFAULT 0,
        completion_tokens INTEGER NOT NULL DEFAULT 0,
        cached_tokens INTEGER NOT NULL DEFAULT 0,
        cost REAL NOT NULL DEFAULT 0.0, created_at TEXT NOT NULL
    );
    CREATE VIRTUAL TABLE messages_fts USING fts5(
        content, role UNINDEXED, conversation_id UNINDEXED, tokenize='unicode61'
    );
    CREATE INDEX idx_usage_events_conversation ON usage_events(conversation_id);
    CREATE INDEX idx_messages_conversation_seq ON messages(conversation_id, seq);
";

/// Migrating a populated v1 database must preserve all rows (no drop-recreate)
/// and bring the schema fully up to date.
#[test]
fn migrate_v1_preserves_user_data() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(V1_SCHEMA).unwrap();
    conn.execute_batch(
        "INSERT INTO conversations (id, started_at, provider, model)
             VALUES (1, '2026-01-01T00:00:00Z', 'openai', 'gpt-4');
         INSERT INTO messages (id, conversation_id, role, content, seq, created_at)
             VALUES (1, 1, 'user', 'hello', 0, '2026-01-01T00:00:00Z');
         INSERT INTO usage_events
             (id, conversation_id, provider, model, prompt_tokens, completion_tokens, cached_tokens, cost, created_at)
             VALUES (1, 1, 'openai', 'gpt-4', 100, 50, 0, 0.01, '2026-01-01T00:00:00Z');",
    )
    .unwrap();
    conn.pragma_update(None, "user_version", 1u32).unwrap();

    let db = SessionDb { conn };
    db.setup_schema().unwrap();

    let version: u32 = db
        .conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 5, "schema should be migrated to latest version");

    // Pre-existing rows survive the migration.
    let conversations: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM conversations", [], |r| r.get(0))
        .unwrap();
    let messages: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!((conversations, messages), (1, 1));

    // The v1->v2 column exists and defaults to 0 for the legacy row.
    let is_estimated: i64 = db
        .conn
        .query_row(
            "SELECT is_estimated FROM usage_events WHERE id = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(is_estimated, 0);

    // The v2->v3 index exists.
    let index_count: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index' AND name = 'idx_usage_events_created_at'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(index_count, 1);

    // The v3->v4 tool_calls column exists.
    let has_tool_calls: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name = 'tool_calls'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(has_tool_calls, 1);
}

/// A non-empty database left at the pre-versioning default (user_version 0)
/// must NOT be silently stamped current — that would skip the is_estimated
/// column. It must error without touching version, data, or schema.
#[test]
fn legacy_unversioned_database_is_not_stamped_current() {
    let conn = Connection::open_in_memory().unwrap();
    // V1_SCHEMA has no is_estimated column; leaving user_version at 0 simulates
    // a database created before schema versioning existed.
    conn.execute_batch(V1_SCHEMA).unwrap();
    conn.execute_batch(
        "INSERT INTO conversations (id, started_at, provider, model)
             VALUES (1, '2026-01-01T00:00:00Z', 'openai', 'gpt-4');",
    )
    .unwrap();

    let db = SessionDb { conn };
    let result = db.setup_schema();
    assert!(result.is_err(), "legacy v0 DB must not migrate silently");

    // Version untouched.
    let version: u32 = db
        .conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 0);

    // Data untouched.
    let count: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM conversations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);

    // Schema untouched: is_estimated was NOT added.
    let has_is_estimated: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('usage_events') WHERE name = 'is_estimated'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(has_is_estimated, 0);
}

/// A fresh database initializes straight to the latest schema.
#[test]
fn fresh_database_initializes_at_latest_version() {
    let conn = Connection::open_in_memory().unwrap();
    let db = SessionDb { conn };
    db.setup_schema().unwrap();

    let version: u32 = db
        .conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 5);

    // record_usage relies on the is_estimated column being present.
    db.create_conversation("openai", "gpt-4").unwrap();
}

#[test]
fn max_message_seq_tracks_highest_seq() {
    let conn = Connection::open_in_memory().unwrap();
    let db = SessionDb { conn };
    db.setup_schema().unwrap();
    let conv = db.create_conversation("openai", "gpt-4").unwrap();

    // No messages yet → 0.
    assert_eq!(db.max_message_seq(conv).unwrap(), 0);

    db.append_message(conv, "user", "hi", None, None, None, None, 1)
        .unwrap();
    db.append_message(conv, "assistant", "hello", None, None, None, None, 2)
        .unwrap();
    assert_eq!(db.max_message_seq(conv).unwrap(), 2);

    // A different conversation is unaffected.
    let other = db.create_conversation("openai", "gpt-4").unwrap();
    assert_eq!(db.max_message_seq(other).unwrap(), 0);
}

#[test]
fn unix_epoch_formats_as_valid_iso_date() {
    assert_eq!(iso_from_unix_secs(0), "1970-01-01T00:00:00Z");
}

#[test]
fn current_era_date_formats_as_valid_iso_date() {
    assert_eq!(iso_from_unix_secs(1_780_745_545), "2026-06-06T11:32:25Z");
    assert_eq!(civil_from_days(20_610), (2026, 6, 6));
}

/// Assistant message with tool_calls JSON is returned by list_messages.
#[test]
fn tool_calls_roundtrip() {
    let conn = Connection::open_in_memory().unwrap();
    let db = SessionDb { conn };
    db.setup_schema().unwrap();
    let conv = db.create_conversation("openai", "gpt-4").unwrap();

    let tc_json = r#"[{"id":"call_1","name":"shell","arguments":"{\"command\":\"ls\",\"env\":[],\"timeout_ms\":120000}"}]"#;
    db.append_message(
        conv,
        "assistant",
        "Let me check.",
        None,
        None,
        Some(tc_json),
        None,
        1,
    )
    .unwrap();

    let msgs = db.list_messages(conv, 100).unwrap();
    assert_eq!(msgs.len(), 1);
    let msg = &msgs[0];
    assert_eq!(msg.content, "Let me check.");
    assert_eq!(msg.tool_calls, Some(tc_json.to_string()));
}

/// Image attachments round-trip through append_message and list_messages.
#[test]
fn images_roundtrip() {
    let conn = Connection::open_in_memory().unwrap();
    let db = SessionDb { conn };
    db.setup_schema().unwrap();
    let conv = db.create_conversation("openai", "gpt-4").unwrap();

    let img_json = r#"[{"media_type":"image/png","data":"aGVsbG8="}]"#;
    db.append_message(conv, "user", "look", None, None, None, Some(img_json), 1)
        .unwrap();

    let msgs = db.list_messages(conv, 100).unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].images, Some(img_json.to_string()));
}

/// Every stats query (snapshot bucket queries + custom range) runs without SQL
/// error against a populated db and aggregates the recorded usage. Guards the
/// shared `SUM_COLS` / `BUCKET_AGG_COLS` / `BUCKET_PROJECTION` SQL fragments.
#[test]
fn usage_stats_queries_aggregate_recorded_usage() {
    let conn = Connection::open_in_memory().unwrap();
    let db = SessionDb { conn };
    db.setup_schema().unwrap();
    let conv = db.create_conversation("openai", "gpt-4").unwrap();
    // A row "now" so today/hourly/range buckets are populated.
    db.record_usage(
        conv,
        "openai",
        "gpt-4",
        100,
        50,
        Some(10),
        Some(0.01),
        false,
    )
    .unwrap();

    let snap = db.usage_stats_snapshot().unwrap();
    assert_eq!(snap.total.prompt_tokens, 100);
    assert_eq!(snap.total.completion_tokens, 50);
    assert_eq!(snap.total.cached_tokens, 10);
    assert_eq!(snap.total.request_count, 1);
    // Bucket queries each return their fixed-width series, not an error.
    assert_eq!(snap.daily.len(), 24, "today is a 24-hour histogram");
    assert!(!snap.weekly.is_empty() && !snap.monthly.is_empty() && !snap.yearly.is_empty());
    assert!(!snap.by_model_today.is_empty());
    // The model breakdown carries the same totals via SUM_COLS.
    let model = &snap.by_model_today[0];
    assert_eq!(model.prompt_tokens, 100);
    assert_eq!(model.completion_tokens, 50);

    // Custom range covering today aggregates the same event.
    let today: String = db
        .conn
        .query_row("SELECT date('now', 'localtime')", [], |r| r.get(0))
        .unwrap();
    let range = db.usage_stats_range(&today, &today).unwrap();
    assert_eq!(range.total.prompt_tokens, 100);
    assert_eq!(range.daily.len(), 1, "single-day range yields one bucket");
    assert_eq!(range.daily[0].prompt_tokens, 100);
}

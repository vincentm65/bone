use super::{
    SCHEMA_VERSION, SessionDb, civil_from_days, db_path_with_legacy, iso_from_unix_secs,
    migrate_legacy_db_if_needed,
};
use rusqlite::Connection;
use std::path::PathBuf;

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
    assert_eq!(
        version, SCHEMA_VERSION,
        "schema should be migrated to latest version"
    );

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

    // The v5->v6 is_error column exists.
    let has_is_error: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name = 'is_error'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(has_is_error, 1);
}

/// A populated pre-versioning database is migrated in place. This must retain
/// every conversation row while adding the columns old schemas lack.
#[test]
fn legacy_unversioned_database_migrates_without_data_loss() {
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
    db.setup_schema().unwrap();

    let version: u32 = db
        .conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);

    // Data untouched.
    let count: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM conversations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);

    let has_is_estimated: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('usage_events') WHERE name = 'is_estimated'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(has_is_estimated, 1);
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
    assert_eq!(version, SCHEMA_VERSION);

    // record_usage relies on the is_estimated column being present.
    db.create_conversation("openai", "gpt-4").unwrap();
}

#[test]
fn v8_migration_adds_context_checkpoints_without_touching_messages() {
    let conn = Connection::open_in_memory().unwrap();
    let db = SessionDb { conn };
    db.setup_schema().unwrap();
    let conv = db.create_conversation("openai", "gpt-4").unwrap();
    db.append_message(conv, "user", "preserved", None, None, None, None, false, 1)
        .unwrap();
    db.conn
        .execute("DROP TABLE conversation_context_checkpoints", [])
        .unwrap();
    db.conn.pragma_update(None, "user_version", 8u32).unwrap();

    db.setup_schema().unwrap();

    let version: u32 = db
        .conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    let checkpoint_table: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'table' AND name = 'conversation_context_checkpoints'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    assert_eq!(checkpoint_table, 1);
    assert_eq!(db.load_messages(conv).unwrap()[0].content, "preserved");
}

#[test]
fn max_message_seq_tracks_highest_seq() {
    let conn = Connection::open_in_memory().unwrap();
    let db = SessionDb { conn };
    db.setup_schema().unwrap();
    let conv = db.create_conversation("openai", "gpt-4").unwrap();

    // No messages yet → 0.
    assert_eq!(db.max_message_seq(conv).unwrap(), 0);

    db.append_message(conv, "user", "hi", None, None, None, None, false, 1)
        .unwrap();
    db.append_message(conv, "assistant", "hello", None, None, None, None, false, 2)
        .unwrap();
    assert_eq!(db.max_message_seq(conv).unwrap(), 2);

    // A different conversation is unaffected.
    let other = db.create_conversation("openai", "gpt-4").unwrap();
    assert_eq!(db.max_message_seq(other).unwrap(), 0);
}

#[test]
fn append_message_repairs_stale_or_duplicate_sequence_hints() {
    let conn = Connection::open_in_memory().unwrap();
    let db = SessionDb { conn };
    db.setup_schema().unwrap();
    let conv = db.create_conversation("openai", "gpt-4").unwrap();

    assert_eq!(
        db.append_message(conv, "user", "one", None, None, None, None, false, 1)
            .unwrap(),
        1
    );
    assert_eq!(
        db.append_message(conv, "assistant", "two", None, None, None, None, false, 1)
            .unwrap(),
        2
    );
    let seqs: Vec<i64> = db
        .list_messages(conv, 10)
        .unwrap()
        .into_iter()
        .map(|m| m.seq)
        .collect();
    assert_eq!(seqs, vec![1, 2]);
}

#[test]
fn append_turn_persists_system_messages() {
    let conn = Connection::open_in_memory().unwrap();
    let db = SessionDb { conn };
    db.setup_schema().unwrap();
    let conv = db.create_conversation("openai", "gpt-4").unwrap();
    let messages = vec![crate::llm::ChatMessage::new(
        crate::llm::ChatRole::System,
        "durable context",
    )];

    assert_eq!(db.append_turn(conv, 0, &messages, &[]).unwrap(), 1);
    let stored = db.list_messages(conv, 10).unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].role, "system");
    assert_eq!(stored[0].content, "durable context");
}

#[test]
fn runtime_load_is_not_truncated_at_history_query_limit() {
    let conn = Connection::open_in_memory().unwrap();
    let db = SessionDb { conn };
    db.setup_schema().unwrap();
    let conv = db.create_conversation("openai", "gpt-4").unwrap();
    let messages: Vec<_> = (0..1001)
        .map(|i| crate::llm::ChatMessage::new(crate::llm::ChatRole::User, format!("message {i}")))
        .collect();
    db.append_turn(conv, 0, &messages, &[]).unwrap();

    assert_eq!(db.list_messages(conv, 2000).unwrap().len(), 1000);
    assert_eq!(db.load_messages(conv).unwrap().len(), 1001);
}

#[test]
fn context_checkpoint_survives_reload_without_rewriting_full_history() {
    let conn = Connection::open_in_memory().unwrap();
    let db = SessionDb { conn };
    db.setup_schema().unwrap();
    let conv = db.create_conversation("openai", "gpt-4").unwrap();
    let original = vec![
        crate::llm::ChatMessage::new(crate::llm::ChatRole::User, "old question"),
        crate::llm::ChatMessage::new(crate::llm::ChatRole::Assistant, "old answer"),
    ];
    db.append_turn(conv, 0, &original, &[]).unwrap();

    let answer = crate::llm::ChatMessage::new(crate::llm::ChatRole::Assistant, "new answer");
    let compacted = vec![
        crate::llm::ChatMessage::new(crate::llm::ChatRole::User, "summary of old context"),
        answer.clone(),
    ];
    db.append_turn_with_checkpoint(conv, 2, &[answer], &[], Some(&compacted))
        .unwrap();
    db.append_message(
        conv,
        "user",
        "after restart",
        None,
        None,
        None,
        None,
        false,
        4,
    )
    .unwrap();

    let full = db.load_messages(conv).unwrap();
    assert_eq!(full.len(), 4);
    assert_eq!(full[0].content, "old question");
    let effective = db.load_effective_transcript(conv).unwrap();
    assert_eq!(effective.len(), 3);
    assert_eq!(effective[0].content, "summary of old context");
    assert_eq!(effective[1].content, "new answer");
    assert_eq!(effective[2].content, "after restart");
}

#[test]
fn checkpoint_rejected_at_save_when_newer_messages_exist() {
    let conn = Connection::open_in_memory().unwrap();
    let db = SessionDb { conn };
    db.setup_schema().unwrap();
    let conv = db.create_conversation("openai", "gpt-4").unwrap();
    db.append_message(conv, "user", "one", None, None, None, None, false, 1)
        .unwrap();
    db.append_message(conv, "user", "concurrent", None, None, None, None, false, 2)
        .unwrap();

    let stale = vec![crate::llm::ChatMessage::new(
        crate::llm::ChatRole::User,
        "summary that missed concurrent",
    )];
    assert!(!db.save_context_checkpoint(conv, 1, &stale).unwrap());
    let effective = db.load_effective_transcript(conv).unwrap();
    assert_eq!(effective.len(), 2);
    assert_eq!(effective[1].content, "concurrent");
}

#[test]
fn malformed_latest_checkpoint_falls_back_to_an_older_revision() {
    let conn = Connection::open_in_memory().unwrap();
    let db = SessionDb { conn };
    db.setup_schema().unwrap();
    let conv = db.create_conversation("openai", "gpt-4").unwrap();
    db.append_message(conv, "user", "original", None, None, None, None, false, 1)
        .unwrap();
    let valid = vec![crate::llm::ChatMessage::new(
        crate::llm::ChatRole::User,
        "valid summary",
    )];
    assert!(db.save_context_checkpoint(conv, 1, &valid).unwrap());
    // Explicitly set a high rowid so the malformed checkpoint is ordered first
    // by the `ORDER BY id DESC` query — makes the test deterministic regardless
    // of insertion timing or `created_at` values.
    let valid_id: i64 = db
        .conn
        .query_row(
            "SELECT id FROM conversation_context_checkpoints WHERE conversation_id = ?1",
            rusqlite::params![conv],
            |r| r.get(0),
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO conversation_context_checkpoints
             (id, conversation_id, through_seq, messages_json, created_at)
             VALUES (?1, ?2, 1, 'not json', '2026-01-01T00:00:00Z')",
            rusqlite::params![valid_id + 1, conv],
        )
        .unwrap();

    let effective = db.load_effective_transcript(conv).unwrap();
    assert_eq!(effective.len(), 1);
    assert_eq!(effective[0].content, "valid summary");
}

#[test]
fn all_checkpoints_malformed_falls_back_to_raw_messages() {
    let conn = Connection::open_in_memory().unwrap();
    let db = SessionDb { conn };
    db.setup_schema().unwrap();
    let conv = db.create_conversation("openai", "gpt-4").unwrap();
    db.append_message(conv, "user", "first", None, None, None, None, false, 1)
        .unwrap();
    db.append_message(
        conv,
        "assistant",
        "second",
        None,
        None,
        None,
        None,
        false,
        2,
    )
    .unwrap();
    db.append_message(
        conv,
        "tool",
        "error result",
        Some("shell"),
        Some("c1"),
        None,
        None,
        true,
        3,
    )
    .unwrap();

    // Insert only malformed checkpoints (invalid JSON).
    for id_offset in [1, 2] {
        db.conn
            .execute(
                "INSERT INTO conversation_context_checkpoints
                 (id, conversation_id, through_seq, messages_json, created_at)
                 VALUES (?1, ?2, ?3, '{{{broken', '2026-01-01T00:00:00Z')",
                rusqlite::params![id_offset, conv, id_offset],
            )
            .unwrap();
    }

    let effective = db.load_effective_transcript(conv).unwrap();
    assert_eq!(effective.len(), 3);
    assert_eq!(effective[0].content, "first");
    assert_eq!(effective[1].content, "second");
    assert_eq!(effective[2].content, "error result");
    assert!(effective[2].is_error, "is_error must survive raw fallback");
}

#[test]
fn v6_migration_rebuilds_drifted_fts_index_without_changing_messages() {
    let conn = Connection::open_in_memory().unwrap();
    let db = SessionDb { conn };
    db.setup_schema().unwrap();
    let conv = db.create_conversation("openai", "gpt-4").unwrap();
    db.append_message(conv, "user", "searchable", None, None, None, None, false, 1)
        .unwrap();
    db.conn.execute("DELETE FROM messages_fts", []).unwrap();
    db.conn
        .execute(
            "INSERT INTO messages_fts(rowid, content, role, conversation_id)
             VALUES (999, 'stale', 'user', 999)",
            [],
        )
        .unwrap();
    db.conn.pragma_update(None, "user_version", 6u32).unwrap();

    db.setup_schema().unwrap();

    let messages: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    let indexed: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM messages m JOIN messages_fts f ON f.rowid = m.id
             WHERE f.content = m.content AND f.role = m.role",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let stale: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM messages_fts WHERE rowid = 999",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!((messages, indexed, stale), (1, 1, 0));
}

#[test]
fn v7_migration_repairs_duplicate_sequences_without_deleting_messages() {
    let conn = Connection::open_in_memory().unwrap();
    let db = SessionDb { conn };
    db.setup_schema().unwrap();
    let conv = db.create_conversation("openai", "gpt-4").unwrap();
    db.conn
        .execute("DROP INDEX idx_messages_conversation_seq_unique", [])
        .unwrap();
    for (id, content) in [(1, "first"), (2, "second"), (3, "third")] {
        db.conn
            .execute(
                "INSERT INTO messages
                 (id, conversation_id, role, content, seq, created_at)
                 VALUES (?1, ?2, 'user', ?3, 1, '2026-01-01T00:00:00Z')",
                rusqlite::params![id, conv, content],
            )
            .unwrap();
    }
    db.conn.pragma_update(None, "user_version", 7u32).unwrap();

    db.setup_schema().unwrap();

    let rows: Vec<(String, i64)> = {
        let mut stmt = db
            .conn
            .prepare("SELECT content, seq FROM messages ORDER BY id")
            .unwrap();
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    assert_eq!(
        rows,
        vec![
            ("first".into(), 1),
            ("second".into(), 2),
            ("third".into(), 3)
        ]
    );
    let duplicate_insert = db.conn.execute(
        "INSERT INTO messages
         (conversation_id, role, content, seq, created_at)
         VALUES (?1, 'user', 'duplicate', 3, '2026-01-01T00:00:01Z')",
        rusqlite::params![conv],
    );
    assert!(
        duplicate_insert.is_err(),
        "unique sequence index must enforce ordering"
    );
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
        false,
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
    db.append_message(
        conv,
        "user",
        "look",
        None,
        None,
        None,
        Some(img_json),
        false,
        1,
    )
    .unwrap();

    let msgs = db.list_messages(conv, 100).unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].images, Some(img_json.to_string()));
}

/// The tool-result error flag round-trips through append_message/list_messages.
#[test]
fn is_error_roundtrip() {
    let conn = Connection::open_in_memory().unwrap();
    let db = SessionDb { conn };
    db.setup_schema().unwrap();
    let conv = db.create_conversation("openai", "gpt-4").unwrap();

    db.append_message(
        conv,
        "tool",
        "boom",
        Some("shell"),
        Some("c1"),
        None,
        None,
        true,
        1,
    )
    .unwrap();
    db.append_message(
        conv,
        "tool",
        "ok",
        Some("shell"),
        Some("c2"),
        None,
        None,
        false,
        2,
    )
    .unwrap();

    let msgs = db.list_messages(conv, 100).unwrap();
    assert_eq!(msgs.len(), 2);
    assert!(
        msgs[0].is_error,
        "errored tool result should persist is_error"
    );
    assert!(!msgs[1].is_error, "successful tool result should not");
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
    let conversation = db.conversation_usage(conv).unwrap();
    assert_eq!(conversation.prompt_tokens, 100);
    assert_eq!(conversation.completion_tokens, 50);
    assert_eq!(conversation.cached_tokens, 10);
    assert_eq!(conversation.request_count, 1);

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

#[test]
fn custom_range_metadata_is_scoped_to_the_requested_dates() {
    let conn = Connection::open_in_memory().unwrap();
    let db = SessionDb { conn };
    db.setup_schema().unwrap();
    let conv = db.create_conversation("openai", "gpt-4").unwrap();
    for (id, created_at) in [
        (1, "2025-01-01T12:00:00Z"),
        (2, "2026-02-03T10:00:00Z"),
        (3, "2026-02-04T11:00:00Z"),
    ] {
        db.conn
            .execute(
                "INSERT INTO usage_events
                 (id, conversation_id, provider, model, prompt_tokens, created_at)
                 VALUES (?1, ?2, 'openai', 'gpt-4', 10, ?3)",
                rusqlite::params![id, conv, created_at],
            )
            .unwrap();
    }

    let range = db.usage_stats_range("2026-02-03", "2026-02-04").unwrap();
    assert!(
        range
            .started_at
            .as_deref()
            .is_some_and(|value| value.starts_with("2026-02-03 "))
    );
    assert!(
        range
            .ended_at
            .as_deref()
            .is_some_and(|value| value.starts_with("2026-02-04 "))
    );
    assert_eq!(range.total.prompt_tokens, 20);
}

#[test]
fn all_time_months_include_usage_older_than_three_years() {
    let conn = Connection::open_in_memory().unwrap();
    let db = SessionDb { conn };
    db.setup_schema().unwrap();
    let conv = db.create_conversation("openai", "gpt-4").unwrap();
    db.conn
        .execute(
            "INSERT INTO usage_events
             (conversation_id, provider, model, prompt_tokens, created_at)
             VALUES (?1, 'openai', 'gpt-4', 42, '2020-01-15T12:00:00Z')",
            rusqlite::params![conv],
        )
        .unwrap();

    let snapshot = db.usage_stats_snapshot().unwrap();
    assert_eq!(snapshot.all_time.first().unwrap().label, "2020-01");
    assert_eq!(
        snapshot
            .range_summary(super::ViewMode::Months)
            .prompt_tokens,
        42
    );
    assert_eq!(snapshot.total.prompt_tokens, 42);
}

#[test]
fn opening_cleanup_prunes_only_ended_fully_empty_conversations() {
    let conn = Connection::open_in_memory().unwrap();
    let db = SessionDb { conn };
    db.setup_schema().unwrap();

    let empty = db.create_conversation("local", "local").unwrap();
    db.conn
        .execute(
            "UPDATE conversations SET ended_at = '2026-01-01T00:00:00Z' WHERE id = ?1",
            [empty],
        )
        .unwrap();
    let kept = db.create_conversation("local", "local").unwrap();
    db.append_message(kept, "user", "keep", None, None, None, None, false, 1)
        .unwrap();
    db.conn
        .execute(
            "UPDATE conversations SET ended_at = '2026-01-01T00:00:00Z' WHERE id = ?1",
            [kept],
        )
        .unwrap();

    assert_eq!(db.prune_ended_empty_conversations().unwrap(), 1);
    assert_eq!(db.latest_conversation().unwrap(), Some((kept, true)));
}

#[test]
fn ending_fully_empty_conversation_deletes_it() {
    let conn = Connection::open_in_memory().unwrap();
    let db = SessionDb { conn };
    db.setup_schema().unwrap();

    let id = db.create_conversation("local", "local").unwrap();
    db.end_conversation(id).unwrap();

    assert_eq!(db.latest_conversation().unwrap(), None);
}

#[test]
fn ending_conversation_with_a_message_preserves_it() {
    let conn = Connection::open_in_memory().unwrap();
    let db = SessionDb { conn };
    db.setup_schema().unwrap();

    let id = db.create_conversation("local", "local").unwrap();
    db.append_message(id, "user", "keep", None, None, None, None, false, 1)
        .unwrap();
    db.end_conversation(id).unwrap();

    assert_eq!(db.latest_conversation().unwrap(), Some((id, true)));
}

/// `latest_conversation` underpins resume-on-boot: it returns the most recent
/// conversation and whether it holds any messages, so `init_db` can reload a
/// non-empty conversation, recycle a trailing empty one, or mint the first.
#[test]
fn latest_conversation_reports_id_and_emptiness() {
    let conn = Connection::open_in_memory().unwrap();
    let db = SessionDb { conn };
    db.setup_schema().unwrap();

    // Empty database: nothing to resume.
    assert_eq!(db.latest_conversation().unwrap(), None);

    // A conversation with a message resumes as non-empty.
    let c1 = db.create_conversation("local", "local").unwrap();
    db.append_message(c1, "user", "hi", None, None, None, None, false, 1)
        .unwrap();
    assert_eq!(db.latest_conversation().unwrap(), Some((c1, true)));

    // A newer, message-less conversation is reported as empty (recyclable).
    let c2 = db.create_conversation("local", "local").unwrap();
    assert_eq!(db.latest_conversation().unwrap(), Some((c2, false)));
    assert!(c2 > c1, "latest is the highest id");
}

/// `db_path` must track `bone_dir()` (XDG/`HOME`), not a hard-coded home path.
/// Serializes env mutations so parallel tests don't race on `XDG_CONFIG_HOME`.
#[test]
fn db_path_follows_bone_dir() {
    let _guard = crate::util::test_env_lock();

    let xdg = std::env::temp_dir().join(format!(
        "bone-db-path-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&xdg).unwrap();

    let old_xdg = std::env::var_os("XDG_CONFIG_HOME");
    let old_bone = std::env::var_os("BONE_DIR");
    // SAFETY: held under test_env_lock; restored below.
    unsafe {
        std::env::remove_var("BONE_DIR");
        std::env::set_var("XDG_CONFIG_HOME", &xdg);
    }

    let expected = xdg.join("bone-rust/data/conversations.db");
    let got = db_path_with_legacy(None);

    match old_bone {
        Some(v) => unsafe { std::env::set_var("BONE_DIR", v) },
        None => unsafe { std::env::remove_var("BONE_DIR") },
    }
    match old_xdg {
        Some(v) => unsafe { std::env::set_var("XDG_CONFIG_HOME", v) },
        None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
    }
    let _ = std::fs::remove_dir_all(&xdg);

    assert_eq!(got, expected);
}

#[test]
fn explicit_bone_dir_does_not_import_legacy_database() {
    let _guard = crate::util::test_env_lock();
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("explicit");
    let legacy = temp.path().join("legacy.db");
    Connection::open(&legacy)
        .unwrap()
        .execute_batch("CREATE TABLE legacy_data (value TEXT);")
        .unwrap();

    let old_bone = std::env::var_os("BONE_DIR");
    unsafe { std::env::set_var("BONE_DIR", &root) };
    let path = db_path_with_legacy(Some(&legacy));
    match old_bone {
        Some(value) => unsafe { std::env::set_var("BONE_DIR", value) },
        None => unsafe { std::env::remove_var("BONE_DIR") },
    }

    assert_eq!(path, root.join("data/conversations.db"));
    assert!(!path.exists());
}

#[test]
fn xdg_migration_uses_independent_sqlite_snapshot_with_wal_data() {
    let _guard = crate::util::test_env_lock();
    let temp = tempfile::tempdir().unwrap();
    let xdg = temp.path().join("xdg");
    let legacy = temp.path().join("legacy/conversations.db");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();

    let source = Connection::open(&legacy).unwrap();
    source
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA wal_autocheckpoint=0;
             CREATE TABLE legacy_data (value TEXT NOT NULL);
             PRAGMA wal_checkpoint(TRUNCATE);
             INSERT INTO legacy_data VALUES ('wal-backed');",
        )
        .unwrap();
    let wal = PathBuf::from(format!("{}-wal", legacy.display()));
    assert!(std::fs::metadata(wal).unwrap().len() > 0);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&legacy, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    let old_bone = std::env::var_os("BONE_DIR");
    let old_xdg = std::env::var_os("XDG_CONFIG_HOME");
    unsafe {
        std::env::remove_var("BONE_DIR");
        std::env::set_var("XDG_CONFIG_HOME", &xdg);
    }
    let migrated = db_path_with_legacy(Some(&legacy));
    match old_bone {
        Some(value) => unsafe { std::env::set_var("BONE_DIR", value) },
        None => unsafe { std::env::remove_var("BONE_DIR") },
    }
    match old_xdg {
        Some(value) => unsafe { std::env::set_var("XDG_CONFIG_HOME", value) },
        None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
    }

    assert_eq!(migrated, xdg.join("bone-rust/data/conversations.db"));
    let destination = Connection::open(&migrated).unwrap();
    let value: String = destination
        .query_row("SELECT value FROM legacy_data", [], |row| row.get(0))
        .unwrap();
    assert_eq!(value, "wal-backed");

    destination
        .execute("UPDATE legacy_data SET value = 'destination'", [])
        .unwrap();
    let source_value: String = source
        .query_row("SELECT value FROM legacy_data", [], |row| row.get(0))
        .unwrap();
    assert_eq!(source_value, "wal-backed");

    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let source_meta = std::fs::metadata(&legacy).unwrap();
        let destination_meta = std::fs::metadata(&migrated).unwrap();
        assert_ne!(source_meta.ino(), destination_meta.ino());
        assert_eq!(destination_meta.permissions().mode() & 0o777, 0o600);
    }

    migrate_legacy_db_if_needed(&legacy, &migrated).unwrap();
    let value: String = destination
        .query_row("SELECT value FROM legacy_data", [], |row| row.get(0))
        .unwrap();
    assert_eq!(value, "destination");
}

#[test]
fn migration_preserves_an_existing_destination() {
    let temp = tempfile::tempdir().unwrap();
    let legacy = temp.path().join("legacy.db");
    let destination = temp.path().join("new/conversations.db");
    std::fs::create_dir_all(destination.parent().unwrap()).unwrap();

    for (path, value) in [(&legacy, "legacy"), (&destination, "destination")] {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch("CREATE TABLE data (value TEXT NOT NULL);")
            .unwrap();
        conn.execute("INSERT INTO data VALUES (?1)", [value])
            .unwrap();
    }

    migrate_legacy_db_if_needed(&legacy, &destination).unwrap();
    let value: String = Connection::open(destination)
        .unwrap()
        .query_row("SELECT value FROM data", [], |row| row.get(0))
        .unwrap();
    assert_eq!(value, "destination");
}

#[test]
fn failed_xdg_migration_keeps_using_legacy_database() {
    let _guard = crate::util::test_env_lock();
    let temp = tempfile::tempdir().unwrap();
    let xdg = temp.path().join("xdg");
    let legacy = temp.path().join("legacy/conversations.db");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    let original = b"not a sqlite database";
    std::fs::write(&legacy, original).unwrap();

    let old_bone = std::env::var_os("BONE_DIR");
    let old_xdg = std::env::var_os("XDG_CONFIG_HOME");
    unsafe {
        std::env::remove_var("BONE_DIR");
        std::env::set_var("XDG_CONFIG_HOME", &xdg);
    }
    let selected = db_path_with_legacy(Some(&legacy));
    match old_bone {
        Some(value) => unsafe { std::env::set_var("BONE_DIR", value) },
        None => unsafe { std::env::remove_var("BONE_DIR") },
    }
    match old_xdg {
        Some(value) => unsafe { std::env::set_var("XDG_CONFIG_HOME", value) },
        None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
    }

    let destination = xdg.join("bone-rust/data/conversations.db");
    assert_eq!(selected, legacy);
    assert!(!destination.exists());
    assert_eq!(std::fs::read(&selected).unwrap(), original);
}

use super::*;

/// The old app's tables, as far as the import reads them.
const OLD_SCHEMA: &str = "
CREATE TABLE conversations (id INTEGER PRIMARY KEY, started_at TEXT NOT NULL, ended_at TEXT,
    provider TEXT NOT NULL, model TEXT NOT NULL, title TEXT);
CREATE TABLE messages (id INTEGER PRIMARY KEY, conversation_id INTEGER NOT NULL, role TEXT NOT NULL,
    content TEXT NOT NULL, tool_name TEXT, tool_call_id TEXT, tool_calls TEXT, images TEXT,
    is_error INTEGER NOT NULL DEFAULT 0, payload_json TEXT, seq INTEGER NOT NULL, created_at TEXT NOT NULL);
CREATE TABLE usage_events (id INTEGER PRIMARY KEY, conversation_id INTEGER NOT NULL, provider TEXT NOT NULL,
    model TEXT NOT NULL, prompt_tokens INTEGER NOT NULL DEFAULT 0, completion_tokens INTEGER NOT NULL DEFAULT 0,
    cached_tokens INTEGER NOT NULL DEFAULT 0, cost REAL NOT NULL DEFAULT 0.0,
    is_estimated INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL);
CREATE TABLE conversation_context_checkpoints (id INTEGER PRIMARY KEY, conversation_id INTEGER NOT NULL,
    through_seq INTEGER NOT NULL, messages_json TEXT NOT NULL, created_at TEXT NOT NULL);
INSERT INTO conversations VALUES (1, '2026-06-07T15:00:00Z', NULL, 'codex', 'gpt-x', NULL);
INSERT INTO conversations VALUES (2, '2026-06-08T10:00:00Z', NULL, 'codex', 'gpt-x', NULL);
INSERT INTO messages (conversation_id, role, content, seq, created_at)
    VALUES (1, 'user', 'fix the walrus', 1, '2026-06-07T15:00:01Z');
INSERT INTO messages (conversation_id, role, content, tool_calls, payload_json, seq, created_at)
    VALUES (1, 'assistant', '', '[{\"id\":\"c1\",\"name\":\"read_file\",\"arguments\":{\"path\":\"/proj/x/src/a.rs\"}}]',
            '{\"version\":2,\"message\":{\"role\":\"assistant\",\"reasoning\":{\"text\":\"look first\"}}}',
            2, '2026-06-07T15:00:02Z');
INSERT INTO messages (conversation_id, role, content, tool_call_id, is_error, payload_json, seq, created_at)
    VALUES (1, 'tool', 'no such file', 'c1', 1, 'not json', 3, '2026-06-07T15:00:03Z');
INSERT INTO messages (conversation_id, role, content, seq, created_at)
    VALUES (1, 'assistant', 'it is gone', 4, '2026-06-07T15:00:04Z');
INSERT INTO messages (conversation_id, role, content, seq, created_at)
    VALUES (1, 'user', 'and now?', 5, '2026-06-07T15:01:00Z');
INSERT INTO messages (conversation_id, role, content, seq, created_at)
    VALUES (1, 'assistant', 'still gone', 6, '2026-06-07T15:01:01Z');
INSERT INTO usage_events (conversation_id, provider, model, prompt_tokens, completion_tokens, cached_tokens, created_at)
    VALUES (1, 'codex', 'gpt-x', 100, 10, 60, '2026-06-07T15:00:02Z'),
           (1, 'codex', 'gpt-x', 200, 20, 150, '2026-06-07T15:01:01Z');
INSERT INTO conversation_context_checkpoints (conversation_id, through_seq, messages_json, created_at)
    VALUES (1, 4, '[{\"version\":2,\"message\":{\"role\":\"user\",\"content\":\"[checkpoint] walrus gone\"}}]',
            '2026-06-07T15:00:30Z');
";

fn old_db(dir: &Path) -> PathBuf {
    let path = dir.join("old.db");
    Connection::open(&path)
        .unwrap()
        .execute_batch(OLD_SCHEMA)
        .unwrap();
    path
}

fn query(data: &Path, sql: &str) -> Json {
    let store = SessionStore::new(data);
    store.index().unwrap().query(sql, &Json::Null).unwrap()["rows"].clone()
}

#[test]
fn conversations_become_sessions_with_usage_and_checkpoints() {
    let dir = tempfile::tempdir().unwrap();
    let db = old_db(dir.path());
    let data = dir.path().join("data");
    let mut seen = Vec::new();
    let report = import_bone1(&db, &data, |done, total| seen.push((done, total))).unwrap();
    assert_eq!(
        report,
        Report {
            imported: 1,
            unchanged: 1,
            messages: 6,
            ..Default::default()
        }
    );
    assert_eq!(seen.last(), Some(&(2, 2)));

    let store = SessionStore::new(&data);
    let list = store.list().unwrap();
    assert_eq!(list.len(), 1);
    let info = &list[0];
    assert_eq!(info.title.as_deref(), Some("fix the walrus"));
    assert_eq!(info.cwd, "/proj/x/src");
    // 2026-06-07T15:00:00Z
    assert_eq!(info.created_at, 1_780_844_400);
    // Resuming starts from the checkpoint, as the old app would.
    let s = store.get(&info.session_id).unwrap();
    let texts: Vec<String> = s
        .lock()
        .unwrap()
        .messages
        .iter()
        .map(|m| match m {
            ChatMessage::User { content, .. } | ChatMessage::Assistant { content, .. } => {
                content.clone()
            }
            other => format!("{other:?}"),
        })
        .collect();
    assert_eq!(
        texts,
        ["[checkpoint] walrus gone", "and now?", "still gone"]
    );
    drop(s);
    drop(store);

    // The whole history is in the index.
    assert_eq!(
        query(
            &data,
            "SELECT turn_id, input_tokens, cached_tokens FROM usage ORDER BY at"
        ),
        serde_json::json!([[1, 100, 60], [2, 200, 150]])
    );
    assert_eq!(
        query(&data, "SELECT name, is_error FROM tool_calls"),
        serde_json::json!([["read_file", 1]])
    );
    assert_eq!(
        query(
            &data,
            "SELECT count(*) FROM search WHERE search MATCH 'walrus'"
        ),
        serde_json::json!([[1]])
    );
}

#[test]
fn importing_again_updates_but_never_overwrites_continued_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let db = old_db(dir.path());
    let data = dir.path().join("data");
    import_bone1(&db, &data, |_, _| {}).unwrap();
    let again = import_bone1(&db, &data, |_, _| {}).unwrap();
    assert_eq!((again.imported, again.unchanged), (0, 2));

    let old = Connection::open(&db).unwrap();
    let add = |seq: i64| {
        old.execute(
            "INSERT INTO messages (conversation_id, role, content, seq, created_at)
             VALUES (1, 'user', 'one more', ?1, '2026-06-07T16:00:00Z')",
            [seq],
        )
        .unwrap();
    };
    add(7);
    let r = import_bone1(&db, &data, |_, _| {}).unwrap();
    assert_eq!((r.updated, r.messages), (1, 7));
    let store = SessionStore::new(&data);
    let id = store.list().unwrap()[0].session_id.clone();
    assert_eq!(store.get(&id).unwrap().lock().unwrap().messages.len(), 4);

    // Continued here: the next import leaves it alone.
    store
        .get(&id)
        .unwrap()
        .lock()
        .unwrap()
        .push(ChatMessage::User {
            content: "continued in bone3".into(),
            images: Vec::new(),
        })
        .unwrap();
    drop(store);
    add(8);
    let r = import_bone1(&db, &data, |_, _| {}).unwrap();
    assert_eq!((r.kept, r.updated), (1, 0));
    let store = SessionStore::new(&data);
    let s = store.get(&id).unwrap();
    let last = s.lock().unwrap().messages.last().cloned();
    assert_eq!(
        last,
        Some(ChatMessage::User {
            content: "continued in bone3".into(),
            images: Vec::new(),
        })
    );
}

#[test]
fn transcripts_are_repaired_for_providers() {
    let call = |id: &str| ToolCall {
        id: id.into(),
        name: "shell".into(),
        arguments: "{}".into(),
    };
    let result = |id: &str| ChatMessage::Tool {
        call_id: id.into(),
        content: "ok".into(),
        is_error: false,
    };
    let user = ChatMessage::User {
        content: "hi".into(),
        images: Vec::new(),
    };
    let asked = ChatMessage::Assistant {
        content: String::new(),
        reasoning: String::new(),
        tool_calls: vec![call("a"), call("b")],
    };
    let fixed = repair(vec![
        result("x"),
        asked.clone(),
        result("b"),
        user.clone(),
        asked.clone(),
    ]);
    let missing = |id: &str| ChatMessage::Tool {
        call_id: id.into(),
        content: NO_RESULT.into(),
        is_error: true,
    };
    assert_eq!(
        fixed,
        [
            asked.clone(),
            result("b"),
            missing("a"),
            user,
            asked,
            missing("a"),
            missing("b"),
        ]
    );
}

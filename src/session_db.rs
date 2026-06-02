use rusqlite::{Connection, params};
use std::path::Path;

/// A search hit from FTS5 query.
pub struct SearchHit {
    pub message_id: i64,
    pub conversation_id: i64,
    pub role: String,
    pub snippet: String,
    pub created_at: String,
}

/// Aggregated usage for one conversation.
pub struct UsageSummary {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cached_tokens: i64,
    pub cost: f64,
    pub request_count: i64,
}

/// Usage broken down by provider/model.
pub struct ProviderUsage {
    pub provider: String,
    pub model: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cached_tokens: i64,
    pub cost: f64,
    pub request_count: i64,
}

/// SQLite-backed conversation and usage storage.
pub struct SessionDb {
    conn: Connection,
}

impl SessionDb {
    /// Open (or create) the database at the given path and run schema setup.
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        }
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")?;
        let db = Self { conn };
        db.setup_schema()?;
        Ok(db)
    }

    /// Open an in-memory database (for tests).
    pub fn open_in_memory() -> rusqlite::Result<Self> {
        let conn = Connection::open_in_memory()?;
        let db = Self { conn };
        db.setup_schema()?;
        Ok(db)
    }

    const SCHEMA_VERSION: u32 = 1;

    fn setup_schema(&self) -> rusqlite::Result<()> {
        let current_version: u32 = self.conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if current_version >= Self::SCHEMA_VERSION {
            return Ok(());
        }
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS conversations (
                id         INTEGER PRIMARY KEY,
                started_at TEXT NOT NULL,
                ended_at   TEXT,
                provider   TEXT NOT NULL,
                model      TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS messages (
                id              INTEGER PRIMARY KEY,
                conversation_id INTEGER NOT NULL REFERENCES conversations(id),
                role            TEXT NOT NULL,
                content         TEXT NOT NULL,
                tool_name       TEXT,
                tool_call_id    TEXT,
                tool_calls      TEXT,
                seq             INTEGER NOT NULL,
                created_at      TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS usage_events (
                id                INTEGER PRIMARY KEY,
                conversation_id   INTEGER NOT NULL REFERENCES conversations(id),
                provider          TEXT NOT NULL,
                model             TEXT NOT NULL,
                prompt_tokens     INTEGER NOT NULL DEFAULT 0,
                completion_tokens INTEGER NOT NULL DEFAULT 0,
                cached_tokens     INTEGER NOT NULL DEFAULT 0,
                cost              REAL    NOT NULL DEFAULT 0.0,
                created_at        TEXT NOT NULL
            );

            CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
                content,
                role UNINDEXED,
                conversation_id UNINDEXED,
                tokenize='unicode61'
            );

            CREATE INDEX IF NOT EXISTS idx_usage_events_conversation
                ON usage_events(conversation_id);

            CREATE INDEX IF NOT EXISTS idx_messages_conversation_seq
                ON messages(conversation_id, seq);
            ",
        )?;
        self.conn.pragma_update(None, "user_version", Self::SCHEMA_VERSION)?;
        Ok(())
    }

    /// Create a new conversation and return its id.
    pub fn create_conversation(&self, provider: &str, model: &str) -> rusqlite::Result<i64> {
        let now = now_iso();
        self.conn.execute(
            "INSERT INTO conversations (started_at, provider, model) VALUES (?1, ?2, ?3)",
            params![now, provider, model],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Append a message to a conversation (inserts into both messages and messages_fts).
    pub fn append_message(
        &self,
        conversation_id: i64,
        role: &str,
        content: &str,
        tool_name: Option<&str>,
        tool_call_id: Option<&str>,
        tool_calls: Option<&str>,
        seq: i64,
    ) -> rusqlite::Result<i64> {
        let now = now_iso();
        self.conn.execute(
            "INSERT INTO messages (conversation_id, role, content, tool_name, tool_call_id, tool_calls, seq, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![conversation_id, role, content, tool_name, tool_call_id, tool_calls, seq, now],
        )?;
        let msg_id = self.conn.last_insert_rowid();
        // Build searchable text: include tool call info in FTS so it's findable
        let searchable = if let Some(tc) = tool_calls {
            format!("{content} TOOL_CALL {tc}")
        } else {
            content.to_string()
        };
        self.conn.execute(
            "INSERT INTO messages_fts (rowid, content, role, conversation_id) VALUES (?1, ?2, ?3, ?4)",
            params![msg_id, searchable, role, conversation_id],
        )?;
        Ok(msg_id)
    }

    /// Record a usage event.
    pub fn record_usage(
        &self,
        conversation_id: i64,
        provider: &str,
        model: &str,
        prompt_tokens: u32,
        completion_tokens: u32,
        cached_tokens: Option<u32>,
        cost: Option<f64>,
    ) -> rusqlite::Result<()> {
        let now = now_iso();
        self.conn.execute(
            "INSERT INTO usage_events (conversation_id, provider, model, prompt_tokens, completion_tokens, cached_tokens, cost, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                conversation_id,
                provider,
                model,
                prompt_tokens as i64,
                completion_tokens as i64,
                cached_tokens.unwrap_or(0) as i64,
                cost.unwrap_or(0.0),
                now,
            ],
        )?;
        Ok(())
    }

    /// Mark a conversation as ended.
    pub fn end_conversation(&self, conversation_id: i64) -> rusqlite::Result<()> {
        let now = now_iso();
        self.conn.execute(
            "UPDATE conversations SET ended_at = ?1 WHERE id = ?2",
            params![now, conversation_id],
        )?;
        Ok(())
    }

    /// Get aggregated usage for a conversation.
    pub fn conversation_usage(&self, conversation_id: i64) -> rusqlite::Result<UsageSummary> {
        let mut stmt = self.conn.prepare(
            "SELECT COALESCE(SUM(prompt_tokens),0), COALESCE(SUM(completion_tokens),0), COALESCE(SUM(cached_tokens),0), COALESCE(SUM(cost),0.0), COUNT(*) FROM usage_events WHERE conversation_id = ?1"
        )?;
        stmt.query_row(params![conversation_id], |row| {
            Ok(UsageSummary {
                prompt_tokens: row.get(0)?,
                completion_tokens: row.get(1)?,
                cached_tokens: row.get(2)?,
                cost: row.get(3)?,
                request_count: row.get(4)?,
            })
        })
    }

    /// Get usage broken down by provider/model for a conversation.
    pub fn usage_by_provider(&self, conversation_id: i64) -> rusqlite::Result<Vec<ProviderUsage>> {
        let mut stmt = self.conn.prepare(
            "SELECT provider, model, COALESCE(SUM(prompt_tokens),0), COALESCE(SUM(completion_tokens),0), COALESCE(SUM(cached_tokens),0), COALESCE(SUM(cost),0.0), COUNT(*) FROM usage_events WHERE conversation_id = ?1 GROUP BY provider, model"
        )?;
        let rows = stmt.query_map(params![conversation_id], |row| {
            Ok(ProviderUsage {
                provider: row.get(0)?,
                model: row.get(1)?,
                prompt_tokens: row.get(2)?,
                completion_tokens: row.get(3)?,
                cached_tokens: row.get(4)?,
                cost: row.get(5)?,
                request_count: row.get(6)?,
            })
        })?;
        rows.collect()
    }

    /// Full-text search across all conversations.
    ///
    /// Uses the raw query with FTS5's implicit OR between terms (`hello world` ->
    /// match "hello" OR "world"). If FTS5 rejects the query as a syntax error
    /// (e.g. unmatched operators), falls back to per-term phrase wrapping so the
    /// user still gets results rather than an error.
    pub fn search(&self, query: &str, limit: i64) -> rusqlite::Result<Vec<SearchHit>> {
        let result = self.try_search(query, limit);
        if let Err(rusqlite::Error::SqliteFailure(_, Some(msg))) = &result
            && msg.contains("syntax error")
        {
            // Fall back: treat as individual quoted terms so special chars
            // don't cause FTS5 parser errors.
            let safe = query
                .split_whitespace()
                .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
                .collect::<Vec<_>>()
                .join(" ");
            return self.try_search(&safe, limit);
        }
        result
    }

    fn try_search(&self, query: &str, limit: i64) -> rusqlite::Result<Vec<SearchHit>> {
        let mut stmt = self.conn.prepare(
            "SELECT m.id, m.conversation_id, m.role, snippet(messages_fts, 0, '▸', '◂', '...', 32) AS snippet, m.created_at
             FROM messages_fts fts
             JOIN messages m ON m.id = fts.rowid
             WHERE messages_fts MATCH ?1
             ORDER BY rank
             LIMIT ?2"
        )?;
        let rows = stmt.query_map(params![query, limit], |row| {
            Ok(SearchHit {
                message_id: row.get(0)?,
                conversation_id: row.get(1)?,
                role: row.get(2)?,
                snippet: row.get(3)?,
                created_at: row.get(4)?,
            })
        })?;
        rows.collect()
    }

    /// Delete a message from both `messages` and `messages_fts`.
    pub fn delete_message(&mut self, message_id: i64) -> rusqlite::Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM messages_fts WHERE rowid = ?1", params![message_id])?;
        tx.execute("DELETE FROM messages WHERE id = ?1", params![message_id])?;
        tx.commit()
    }
}

fn now_iso() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = secs / 86400;
    let tod = secs % 86400;
    let (y, m, d) = civil_from_days(days as i64);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m,
        d,
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

/// Convert days since 1970-01-01 to (year, month, day) using
/// Howard Hinnant's civil-from-days algorithm.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097); // [0, 146096]
    let yoe = (doe - doe / 4 + doe / 100 - doe / 400) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100 + yoe / 400);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> SessionDb {
        SessionDb::open_in_memory().unwrap()
    }

    #[test]
    fn create_and_end_conversation() {
        let db = test_db();
        let id = db.create_conversation("test", "model-1").unwrap();
        assert!(id > 0);
        db.end_conversation(id).unwrap();

        let usage = db.conversation_usage(id).unwrap();
        assert_eq!(usage.request_count, 0);
    }

    #[test]
    fn append_and_retrieve_messages() {
        let db = test_db();
        let conv_id = db.create_conversation("test", "m").unwrap();
        db.append_message(conv_id, "user", "Hello", None, None, None, 0)
            .unwrap();
        db.append_message(conv_id, "assistant", "Hi there", None, None, None, 1)
            .unwrap();
        db.append_message(conv_id, "tool", "result", Some("read_file"), Some("call-1"), None, 2)
            .unwrap();

        let mut stmt = db
            .conn
            .prepare(
                "SELECT role, content, tool_name, seq FROM messages WHERE conversation_id = ?1 ORDER BY seq",
            )
            .unwrap();
        let rows: Vec<(String, String, Option<String>, i64)> = stmt
            .query_map(params![conv_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].0, "user");
        assert_eq!(rows[1].0, "assistant");
        assert_eq!(rows[2].2.as_deref(), Some("read_file"));
    }

    #[test]
    fn append_message_populates_fts() {
        let db = test_db();
        let conv_id = db.create_conversation("test", "m").unwrap();
        db.append_message(
            conv_id,
            "user",
            "The quick brown fox jumps over the lazy dog",
            None, None, None,
            0,
        )
            .unwrap();
        let hits = db.search("brown fox", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].role, "user");
    }

    #[test]
    fn delete_message_removes_from_both() {
        let mut db = test_db();
        let conv_id = db.create_conversation("test", "m").unwrap();
        let msg_id = db
            .append_message(conv_id, "user", "unique searchable content here", None, None, None, 0)
            .unwrap();

        // Verify it's there
        assert_eq!(db.search("unique searchable", 10).unwrap().len(), 1);

        db.delete_message(msg_id).unwrap();

        // Verify it's gone from FTS
        assert_eq!(db.search("unique searchable", 10).unwrap().len(), 0);

        // Verify it's gone from messages
        let count: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE id = ?1",
                params![msg_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn record_and_sum_usage() {
        let db = test_db();
        let conv_id = db.create_conversation("test", "m").unwrap();
        db.record_usage(conv_id, "test", "m", 100, 50, Some(20), Some(0.01))
            .unwrap();
        db.record_usage(conv_id, "test", "m", 200, 80, None, None)
            .unwrap();

        let usage = db.conversation_usage(conv_id).unwrap();
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
        db.record_usage(conv_id, "glm", "glm-4", 1000, 100, Some(500), Some(0.1))
            .unwrap();
        db.record_usage(conv_id, "openrouter", "claude-3", 2000, 200, None, Some(0.2))
            .unwrap();
        db.record_usage(conv_id, "glm", "glm-4", 500, 50, None, None)
            .unwrap();

        let by_provider = db.usage_by_provider(conv_id).unwrap();
        assert_eq!(by_provider.len(), 2);

        let glm = by_provider
            .iter()
            .find(|p| p.provider == "glm")
            .unwrap();
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
            None, None, None,
            0,
        )
            .unwrap();
        db.append_message(
            conv_id,
            "assistant",
            "Better model: usage is recorded per request",
            None, None, None,
            1,
        )
            .unwrap();
        let hits = db.search("provider switch", 10).unwrap();
        assert!(!hits.is_empty());
        assert!(hits[0].snippet.contains("\u{25b8}"));
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
}

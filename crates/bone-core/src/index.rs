//! The session index: a SQLite database over the session files, for
//! listing, searching and counting what sessions did (`store/query`).
//!
//! The session files stay the record. The index follows each one by
//! reading what was appended since it last looked, so the same code indexes
//! a live turn, catches up at startup, and rebuilds from nothing — delete
//! `index.db` and it comes back. It holds no transcript copies beyond the
//! user and assistant text kept for search.

use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use bone_proto::types::ChatMessage;
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};
use serde_json::{Value as Json, json};

use crate::session::{Line, Record, title_of};

/// Bump when the tables change; an index of another version is rebuilt.
const VERSION: i64 = 4;

const SCHEMA: &str = "
CREATE TABLE sessions (
    id          TEXT PRIMARY KEY,
    parent      TEXT,
    cwd         TEXT NOT NULL,
    title       TEXT,               -- the first user message
    renamed     TEXT,               -- a title given with session/rename
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    messages    INTEGER NOT NULL DEFAULT 0
);
-- How far each file is indexed.
CREATE TABLE files (
    path        TEXT PRIMARY KEY,
    session_id  TEXT NOT NULL,      -- '' for usage.jsonl
    indexed_len INTEGER NOT NULL,
    messages    INTEGER NOT NULL,
    last_at     INTEGER NOT NULL
);
CREATE TABLE messages (
    session_id TEXT NOT NULL,
    seq        INTEGER NOT NULL,    -- 1-based, in file order
    at         INTEGER NOT NULL,
    role       TEXT NOT NULL,       -- system, user, assistant, tool
    chars      INTEGER NOT NULL,
    PRIMARY KEY (session_id, seq)
) WITHOUT ROWID;
CREATE INDEX messages_at ON messages(at);
CREATE VIRTUAL TABLE search USING fts5(
    text, session_id UNINDEXED, seq UNINDEXED, role UNINDEXED, at UNINDEXED
);
CREATE TABLE usage (
    session_id    TEXT NOT NULL,    -- '' for calls outside any session
    turn_id       INTEGER NOT NULL, -- 0 for calls outside a turn
    source        TEXT NOT NULL,    -- turn, lua or client
    at            INTEGER NOT NULL,
    provider      TEXT,
    model         TEXT NOT NULL,
    input_tokens  INTEGER NOT NULL,
    output_tokens INTEGER NOT NULL,
    cached_tokens INTEGER NOT NULL DEFAULT 0,
    context_tokens INTEGER
);
CREATE INDEX usage_at ON usage(at);
CREATE INDEX usage_session ON usage(session_id);
CREATE TABLE tool_calls (
    session_id   TEXT NOT NULL,
    seq          INTEGER NOT NULL,  -- the assistant message that made it
    at           INTEGER NOT NULL,
    call_id      TEXT NOT NULL,
    name         TEXT NOT NULL,
    is_error     INTEGER,           -- NULL until its result is in
    output_chars INTEGER
);
CREATE INDEX tool_calls_session ON tool_calls(session_id, call_id);
CREATE INDEX tool_calls_at ON tool_calls(at);
";

const TABLES: [&str; 6] = [
    "sessions",
    "files",
    "messages",
    "search",
    "usage",
    "tool_calls",
];

/// Rows one query may return.
const MAX_ROWS: usize = 10_000;
/// How long one query may run.
const QUERY_TIME: Duration = Duration::from_secs(5);

pub struct Index {
    path: PathBuf,
    conn: Mutex<Connection>,
}

type Result<T> = std::result::Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

impl Index {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(err)?;
        }
        let conn = Connection::open(path).map_err(err)?;
        conn.busy_timeout(Duration::from_secs(5)).map_err(err)?;
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")
            .map_err(err)?;
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(err)?;
        if version == 3 {
            conn.execute_batch(
                "BEGIN; ALTER TABLE usage ADD COLUMN context_tokens INTEGER; PRAGMA user_version = 4; COMMIT;",
            ).map_err(err)?;
        } else if version != VERSION {
            let mut drop = String::new();
            for t in TABLES {
                drop.push_str(&format!("DROP TABLE IF EXISTS {t};"));
            }
            conn.execute_batch(&format!(
                "BEGIN; {drop} {SCHEMA} PRAGMA user_version = {VERSION}; COMMIT;"
            ))
            .map_err(err)?;
        }
        Ok(Index {
            path: path.to_owned(),
            conn: Mutex::new(conn),
        })
    }

    /// Index what was appended to session `id`'s file since last time. A
    /// file that shrank, or was never seen, is indexed from the start.
    /// `renamed` sets the title a rename gave.
    pub fn sync(&self, id: &str, path: &Path, renamed: Option<&str>) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(err)?;
        let file = path.to_string_lossy();
        let known: Option<(u64, u64, u64)> = tx
            .query_row(
                "SELECT indexed_len, messages, last_at FROM files WHERE path = ?1",
                [&file],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(err)?;
        let len = std::fs::metadata(path).map_err(err)?.len();
        let (mut offset, mut seq, mut last_at) = known.unwrap_or_default();
        if known.is_none() || offset > len {
            clear(&tx, id, false)?;
            (offset, seq) = (0, 0);
        }
        if offset < len {
            let mut file = File::open(path).map_err(err)?;
            file.seek(SeekFrom::Start(offset)).map_err(err)?;
            let mut reader = BufReader::new(file);
            let mut text = String::new();
            loop {
                text.clear();
                let n = reader.read_line(&mut text).map_err(err)?;
                // Stop at the end, or at a line still being written.
                if n == 0 || !text.ends_with('\n') {
                    break;
                }
                offset += n as u64;
                let Ok(line) = serde_json::from_str::<Line>(&text) else {
                    continue;
                };
                let at = line.at.unwrap_or(last_at);
                last_at = last_at.max(at);
                index_record(&tx, id, line.record, at, &mut seq)?;
            }
        }
        tx.execute(
            "INSERT OR REPLACE INTO files (path, session_id, indexed_len, messages, last_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            params![file, id, offset, seq, last_at],
        )
        .map_err(err)?;
        tx.execute(
            "UPDATE sessions SET messages = ?2, updated_at = max(updated_at, ?3) WHERE id = ?1",
            params![id, seq, last_at],
        )
        .map_err(err)?;
        if let Some(title) = renamed {
            tx.execute(
                "UPDATE sessions SET renamed = ?2 WHERE id = ?1",
                params![id, title],
            )
            .map_err(err)?;
        }
        tx.commit().map_err(err)
    }

    pub fn rename(&self, id: &str, title: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE sessions SET renamed = ?2 WHERE id = ?1",
            params![id, title],
        )
        .map(drop)
        .map_err(err)
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(err)?;
        clear(&tx, id, true)?;
        tx.commit().map_err(err)
    }

    /// Drop sessions whose files are gone.
    pub fn retain(&self, ids: &[String]) -> Result<()> {
        let stale: Vec<String> = {
            let conn = self.conn.lock().unwrap();
            let mut stmt = conn.prepare("SELECT id FROM sessions").map_err(err)?;
            stmt.query_map([], |r| r.get::<_, String>(0))
                .map_err(err)?
                .filter_map(|r| r.ok())
                .filter(|id| !ids.contains(id))
                .collect()
        };
        for id in stale {
            self.remove(&id)?;
        }
        Ok(())
    }

    /// Run one read-only SQL statement. `params` is a list (`?1`, `?2`…) or
    /// an object (`:name`). At most [`MAX_ROWS`] rows come back; `truncated`
    /// says there were more.
    pub fn query(&self, sql: &str, params: &Json) -> Result<Json> {
        let conn = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(err)?;
        conn.busy_timeout(Duration::from_secs(5)).map_err(err)?;
        let deadline = Instant::now() + QUERY_TIME;
        conn.progress_handler(10_000, Some(move || Instant::now() > deadline));
        let mut stmt = conn.prepare(sql).map_err(err)?;
        let columns: Vec<String> = stmt.column_names().iter().map(|c| c.to_string()).collect();
        let mut rows = match params {
            Json::Null => stmt.query([]),
            Json::Array(list) => {
                let values: Vec<rusqlite::types::Value> = list.iter().map(to_sql).collect();
                stmt.query(rusqlite::params_from_iter(values))
            }
            Json::Object(map) => {
                let named: Vec<(String, rusqlite::types::Value)> = map
                    .iter()
                    .map(|(k, v)| {
                        let k = if k.starts_with([':', '@', '$']) {
                            k.clone()
                        } else {
                            format!(":{k}")
                        };
                        (k, to_sql(v))
                    })
                    .collect();
                let refs: Vec<(&str, &dyn rusqlite::ToSql)> = named
                    .iter()
                    .map(|(k, v)| (k.as_str(), v as &dyn rusqlite::ToSql))
                    .collect();
                stmt.query(refs.as_slice())
            }
            _ => return Err("params must be a list or an object".into()),
        }
        .map_err(err)?;
        let mut out = Vec::new();
        let mut truncated = false;
        while let Some(row) = rows.next().map_err(|e| match e {
            rusqlite::Error::SqliteFailure(f, _)
                if f.code == rusqlite::ErrorCode::OperationInterrupted =>
            {
                format!("the query ran longer than {}s", QUERY_TIME.as_secs())
            }
            e => err(e),
        })? {
            if out.len() == MAX_ROWS {
                truncated = true;
                break;
            }
            let cells: Vec<Json> = (0..columns.len())
                .map(|i| row.get_ref(i).map(from_sql).unwrap_or(Json::Null))
                .collect();
            out.push(Json::Array(cells));
        }
        Ok(json!({ "columns": columns, "rows": out, "truncated": truncated }))
    }
}

/// Remove a session's rows; its `sessions` row too when `all`, else just
/// what is rebuilt from the file.
fn clear(tx: &Transaction, id: &str, all: bool) -> Result<()> {
    for t in ["files", "messages", "search", "usage", "tool_calls"] {
        tx.execute(&format!("DELETE FROM {t} WHERE session_id = ?1"), [id])
            .map_err(err)?;
    }
    let sql = if all {
        "DELETE FROM sessions WHERE id = ?1"
    } else {
        "UPDATE sessions SET title = NULL, messages = 0 WHERE id = ?1"
    };
    tx.execute(sql, [id]).map(drop).map_err(err)
}

fn index_record(tx: &Transaction, id: &str, record: Record, at: u64, seq: &mut u64) -> Result<()> {
    let run = |sql: &str, p: &[&dyn rusqlite::ToSql]| tx.execute(sql, p).map(drop).map_err(err);
    match record {
        Record::Session(h) => run(
            "INSERT INTO sessions (id, parent, cwd, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT(id) DO UPDATE SET parent = ?2, cwd = ?3, created_at = ?4",
            params![id, h.parent, h.cwd, h.created_at],
        ),
        Record::Compact(_) | Record::Summary(_) | Record::Model(_) => Ok(()),
        Record::Usage(u) => run(
            "INSERT INTO usage (session_id, turn_id, source, at, provider, model,
                                input_tokens, output_tokens, cached_tokens, context_tokens)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                id,
                u.turn_id,
                u.source.as_deref().unwrap_or("turn"),
                at,
                u.provider,
                u.model,
                u.input_tokens,
                u.output_tokens,
                u.cached_tokens.unwrap_or(0),
                u.context_tokens
            ],
        ),
        Record::Message(m) => {
            *seq += 1;
            let seq = *seq;
            let (role, text) = match &m {
                ChatMessage::System { content } => ("system", content),
                ChatMessage::User { content } => ("user", content),
                ChatMessage::Assistant { content, .. } => ("assistant", content),
                ChatMessage::Tool { content, .. } => ("tool", content),
            };
            run(
                "INSERT OR REPLACE INTO messages (session_id, seq, at, role, chars)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, seq, at, role, text.chars().count()],
            )?;
            if matches!(role, "user" | "assistant") && !text.trim().is_empty() {
                run(
                    "INSERT INTO search (text, session_id, seq, role, at) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![text, id, seq, role, at],
                )?;
            }
            match m {
                ChatMessage::User { .. } => {
                    if let Some(title) = title_of(&m) {
                        run(
                            "UPDATE sessions SET title = ?2 WHERE id = ?1 AND title IS NULL",
                            params![id, title],
                        )?;
                    }
                }
                ChatMessage::Assistant { tool_calls, .. } => {
                    for c in tool_calls {
                        run(
                            "INSERT INTO tool_calls (session_id, seq, at, call_id, name)
                                 VALUES (?1, ?2, ?3, ?4, ?5)",
                            params![id, seq, at, c.id, c.name],
                        )?;
                    }
                }
                ChatMessage::Tool {
                    call_id,
                    content,
                    is_error,
                } => run(
                    "UPDATE tool_calls SET is_error = ?3, output_chars = ?4
                         WHERE session_id = ?1 AND call_id = ?2 AND is_error IS NULL",
                    params![id, call_id, is_error, content.chars().count()],
                )?,
                ChatMessage::System { .. } => {}
            }
            Ok(())
        }
    }
}

fn to_sql(v: &Json) -> rusqlite::types::Value {
    use rusqlite::types::Value;
    match v {
        Json::Null => Value::Null,
        Json::Bool(b) => Value::Integer(i64::from(*b)),
        Json::Number(n) => match n.as_i64() {
            Some(i) => Value::Integer(i),
            None => Value::Real(n.as_f64().unwrap_or_default()),
        },
        Json::String(s) => Value::Text(s.clone()),
        other => Value::Text(other.to_string()),
    }
}

fn from_sql(v: rusqlite::types::ValueRef) -> Json {
    use rusqlite::types::ValueRef;
    match v {
        ValueRef::Null => Json::Null,
        ValueRef::Integer(i) => json!(i),
        ValueRef::Real(f) => json!(f),
        ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
        ValueRef::Blob(b) => json!(format!("<{} bytes>", b.len())),
    }
}

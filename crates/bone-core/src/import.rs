//! Import conversations from the first bone (its SQLite database,
//! `~/.bone-rust/data/conversations.db`) as sessions.
//!
//! Each conversation becomes a session file with the same messages and
//! times, its usage records, and its latest context checkpoint as a
//! compaction, so resuming it sends what the old app would have. The old
//! database stores no working directory; it is guessed from the absolute
//! paths the conversation's tools used.
//!
//! Running it again imports new conversations and brings imported ones up
//! to date, but never touches a session that was continued (or deleted)
//! here. What was imported is kept in `<data_dir>/imported/bone1.json`.

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};

use bone_proto::types::{ChatMessage, ToolCall};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::session::{Header, Line, Record, SessionStore, UsageRecord};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub imported: usize,
    pub updated: usize,
    /// Already imported and unchanged, or without messages.
    pub unchanged: usize,
    /// Imported before and since continued or deleted here: left alone.
    pub kept: usize,
    pub messages: usize,
}

/// What was imported from one conversation.
#[derive(Serialize, Deserialize)]
struct Imported {
    session_id: String,
    last_seq: i64,
    /// The file's length when written, to notice it was continued.
    len: u64,
}

type Marker = BTreeMap<i64, Imported>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Import every conversation in `db` into `data_dir`; `progress(done,
/// total)` follows along. The session index is brought up to date at the
/// end.
pub fn import_bone1(
    db: &Path,
    data_dir: &Path,
    mut progress: impl FnMut(usize, usize),
) -> Result<Report, String> {
    let conn = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("cannot open {}: {e}", db.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(10))
        .map_err(err)?;
    let sessions = data_dir.join("sessions");
    std::fs::create_dir_all(&sessions).map_err(err)?;
    let marker_path = data_dir.join("imported/bone1.json");
    let mut marker: Marker = std::fs::read_to_string(&marker_path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();

    let conversations: Vec<(i64, i64)> = conn
        .prepare(
            "SELECT id, CAST(strftime('%s', started_at) AS INTEGER) FROM conversations ORDER BY id",
        )
        .and_then(|mut s| {
            s.query_map([], |r| {
                Ok((r.get(0)?, r.get::<_, Option<i64>>(1)?.unwrap_or(0)))
            })?
            .collect()
        })
        .map_err(err)?;
    let mut report = Report::default();
    let total = conversations.len();
    for (n, (id, started)) in conversations.into_iter().enumerate() {
        progress(n, total);
        let last_seq: Option<i64> = conn
            .query_row(
                "SELECT max(seq) FROM messages WHERE conversation_id = ?1",
                [id],
                |r| r.get(0),
            )
            .map_err(err)?;
        let Some(last_seq) = last_seq else {
            report.unchanged += 1;
            continue;
        };
        let session_id = match marker.get(&id) {
            Some(done) => {
                let path = sessions.join(format!("{}.jsonl", done.session_id));
                match std::fs::metadata(&path) {
                    Ok(m) if m.len() != done.len => {
                        report.kept += 1;
                        continue;
                    }
                    Err(_) => {
                        report.kept += 1;
                        continue;
                    }
                    Ok(_) if done.last_seq == last_seq => {
                        report.unchanged += 1;
                        continue;
                    }
                    Ok(_) => {
                        report.updated += 1;
                        done.session_id.clone()
                    }
                }
            }
            None => {
                report.imported += 1;
                session_id(id, started)
            }
        };
        let (text, count) = convert(&conn, id, started, &session_id)?;
        let path = sessions.join(format!("{session_id}.jsonl"));
        let tmp = path.with_extension("jsonl.importing");
        std::fs::write(&tmp, &text).map_err(err)?;
        std::fs::rename(&tmp, &path).map_err(err)?;
        report.messages += count;
        marker.insert(
            id,
            Imported {
                session_id,
                last_seq,
                len: text.len() as u64,
            },
        );
    }
    progress(total, total);
    if let Some(dir) = marker_path.parent() {
        std::fs::create_dir_all(dir).map_err(err)?;
    }
    let text = serde_json::to_string_pretty(&marker).map_err(err)?;
    std::fs::write(&marker_path, text).map_err(err)?;
    SessionStore::new(data_dir).catch_up();
    Ok(report)
}

/// A UUIDv7 at the conversation's start, so sessions sort by time, with the
/// rest taken from its id, so importing again finds the same session.
fn session_id(conversation: i64, started: i64) -> String {
    let mut rest = [0u8; 10];
    rest[..2].copy_from_slice(b"b1");
    rest[2..].copy_from_slice(&conversation.to_be_bytes());
    let millis = u64::try_from(started).unwrap_or(0) * 1000;
    uuid::Builder::from_unix_timestamp_millis(millis, &rest)
        .into_uuid()
        .to_string()
}

struct Row {
    seq: i64,
    at: u64,
    message: ChatMessage,
}

/// One conversation as the lines of a session file, and its message count.
fn convert(
    conn: &Connection,
    id: i64,
    started: i64,
    session_id: &str,
) -> Result<(String, usize), String> {
    let started = u64::try_from(started).unwrap_or(0);
    let rows: Vec<Row> = conn
        .prepare(
            "SELECT seq, role, content, tool_calls, tool_call_id, is_error,
                    CAST(strftime('%s', created_at) AS INTEGER),
                    CASE WHEN role = 'assistant' AND json_valid(payload_json)
                         THEN json_extract(payload_json, '$.message.reasoning.text') END
             FROM messages WHERE conversation_id = ?1 ORDER BY seq",
        )
        .and_then(|mut s| {
            s.query_map([id], |r| {
                let role: String = r.get(1)?;
                let calls: Option<String> = r.get(3)?;
                let calls = calls.and_then(|c| serde_json::from_str(&c).ok());
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Option<i64>>(6)?,
                    message(
                        &role,
                        r.get::<_, String>(2)?,
                        calls.as_ref(),
                        r.get::<_, Option<String>>(4)?,
                        r.get::<_, bool>(5)?,
                        r.get::<_, Option<String>>(7)?,
                    ),
                ))
            })?
            .collect::<Result<Vec<_>, _>>()
        })
        .map_err(err)?
        .into_iter()
        .filter_map(|(seq, at, m)| {
            Some(Row {
                seq,
                at: at.and_then(|a| u64::try_from(a).ok()).unwrap_or(started),
                message: m?,
            })
        })
        .collect();

    let usage: Vec<(u64, UsageRecord)> = conn
        .prepare(
            "SELECT CAST(strftime('%s', created_at) AS INTEGER), provider, model,
                    prompt_tokens, completion_tokens, cached_tokens
             FROM usage_events WHERE conversation_id = ?1 ORDER BY id",
        )
        .and_then(|mut s| {
            s.query_map([id], |r| {
                Ok((
                    r.get::<_, Option<i64>>(0)?.unwrap_or(0).max(0) as u64,
                    UsageRecord {
                        turn_id: 0,
                        provider: Some(r.get(1)?),
                        model: r.get(2)?,
                        input_tokens: r.get::<_, i64>(3)?.max(0) as u64,
                        output_tokens: r.get::<_, i64>(4)?.max(0) as u64,
                        context_tokens: None,
                        cached_tokens: Some(r.get::<_, i64>(5)?.max(0) as u64),
                        source: None,
                    },
                ))
            })?
            .collect()
        })
        .map_err(err)?;

    // The latest checkpoint stands for everything up to its message.
    let checkpoint: Option<(i64, Vec<ChatMessage>)> = conn
        .query_row(
            "SELECT through_seq, messages_json FROM conversation_context_checkpoints
             WHERE conversation_id = ?1 ORDER BY id DESC LIMIT 1",
            [id],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(err)?
        .map(|(through, text)| (through, checkpoint_messages(&text)));

    let header = Header {
        session_id: session_id.to_owned(),
        cwd: guess_cwd(&rows),
        created_at: started,
        parent: None,
        owner: None,
    };
    let mut out = Vec::new();
    let mut put = |record: Record, at: u64| {
        let line = Line {
            record,
            at: Some(at),
        };
        if let Ok(mut l) = serde_json::to_vec(&line) {
            l.push(b'\n');
            out.write_all(&l).ok();
        }
    };
    put(Record::Session(header), started);
    let mut usage = usage.into_iter().peekable();
    let mut checkpoint = checkpoint;
    let mut turn = 0;
    let mut closer = Closer::default();
    for row in &rows {
        while let Some((at, _)) = usage.peek()
            && *at < row.at
        {
            let (at, mut u) = usage.next().unwrap();
            u.turn_id = turn;
            put(Record::Usage(u), at);
        }
        if let Some((through, _)) = &checkpoint
            && row.seq > *through
        {
            let (_, messages) = checkpoint.take().unwrap();
            for m in closer.close() {
                put(Record::Message(m), row.at);
            }
            put(Record::Compact(repair(messages)), row.at);
        }
        if matches!(row.message, ChatMessage::User { .. }) {
            turn += 1;
        }
        for m in closer.next(row.message.clone()) {
            put(Record::Message(m), row.at);
        }
    }
    let last = rows.last().map_or(started, |r| r.at);
    for m in closer.close() {
        put(Record::Message(m), last);
    }
    for (at, mut u) in usage {
        u.turn_id = turn;
        put(Record::Usage(u), at.max(last));
    }
    if let Some((_, messages)) = checkpoint {
        put(Record::Compact(repair(messages)), last);
    }
    let text = String::from_utf8(out).map_err(err)?;
    Ok((text, rows.len()))
}

const NO_RESULT: &str = "[no result: the turn ended before this tool call finished]";

/// Keeps a transcript as providers want it: each tool call answered before
/// anything else comes, and no result without its call. The old app's
/// cancelled turns and checkpoints left both kinds of gap.
#[derive(Default)]
struct Closer {
    pending: Vec<String>,
}

impl Closer {
    /// What to write for `m`: results still missing first; nothing for a
    /// result whose call is not open.
    fn next(&mut self, m: ChatMessage) -> Vec<ChatMessage> {
        if let ChatMessage::Tool { call_id, .. } = &m {
            return match self.pending.iter().position(|c| c == call_id) {
                Some(i) => {
                    self.pending.remove(i);
                    vec![m]
                }
                None => Vec::new(),
            };
        }
        let mut out = self.close();
        if let ChatMessage::Assistant { tool_calls, .. } = &m {
            self.pending = tool_calls.iter().map(|c| c.id.clone()).collect();
        }
        out.push(m);
        out
    }

    /// Results for the calls still open.
    fn close(&mut self) -> Vec<ChatMessage> {
        self.pending
            .drain(..)
            .map(|call_id| ChatMessage::Tool {
                call_id,
                content: NO_RESULT.into(),
                is_error: true,
            })
            .collect()
    }
}

fn repair(messages: Vec<ChatMessage>) -> Vec<ChatMessage> {
    let mut closer = Closer::default();
    let mut out: Vec<ChatMessage> = messages.into_iter().flat_map(|m| closer.next(m)).collect();
    out.extend(closer.close());
    out
}

/// A message from the old app's columns (or payload fields).
fn message(
    role: &str,
    content: String,
    calls: Option<&Json>,
    call_id: Option<String>,
    is_error: bool,
    reasoning: Option<String>,
) -> Option<ChatMessage> {
    Some(match role {
        "user" => ChatMessage::User { content },
        "system" => ChatMessage::System { content },
        "assistant" => ChatMessage::Assistant {
            content,
            reasoning: reasoning.unwrap_or_default(),
            tool_calls: calls
                .and_then(Json::as_array)
                .map(|list| {
                    list.iter()
                        .map(|c| ToolCall {
                            id: c["id"].as_str().unwrap_or_default().to_owned(),
                            name: c["name"].as_str().unwrap_or_default().to_owned(),
                            arguments: match &c["arguments"] {
                                Json::String(s) => s.clone(),
                                Json::Null => "{}".into(),
                                other => other.to_string(),
                            },
                        })
                        .collect()
                })
                .unwrap_or_default(),
        },
        "tool" => ChatMessage::Tool {
            call_id: call_id.unwrap_or_default(),
            content,
            is_error,
        },
        _ => return None,
    })
}

/// A checkpoint's messages, stored as `[{ version, message }]`.
fn checkpoint_messages(text: &str) -> Vec<ChatMessage> {
    let entries: Vec<Json> = serde_json::from_str(text).unwrap_or_default();
    entries
        .iter()
        .filter_map(|e| {
            let m = &e["message"];
            message(
                m["role"].as_str()?,
                m["content"].as_str().unwrap_or_default().to_owned(),
                Some(&m["tool_calls"]),
                m["tool_call_id"].as_str().map(str::to_owned),
                m["is_error"].as_bool().unwrap_or(false),
                m["reasoning"]["text"].as_str().map(str::to_owned),
            )
        })
        .collect()
}

/// The directory most of the conversation's absolute tool paths are in
/// (or the git repository around it); the home directory when there is
/// none.
fn guess_cwd(rows: &[Row]) -> String {
    let mut paths: Vec<PathBuf> = Vec::new();
    for row in rows {
        let ChatMessage::Assistant { tool_calls, .. } = &row.message else {
            continue;
        };
        for c in tool_calls {
            let Ok(args) = serde_json::from_str::<Json>(&c.arguments) else {
                continue;
            };
            for key in ["path", "file_path", "cwd", "workdir", "dir"] {
                if let Some(p) = args[key].as_str().filter(|p| p.starts_with('/')) {
                    paths.push(PathBuf::from(p));
                }
            }
        }
    }
    let mut counts: HashMap<&Path, usize> = HashMap::new();
    for p in &paths {
        for a in p.ancestors().skip(1) {
            *counts.entry(a).or_default() += 1;
        }
    }
    let needed = (paths.len() * 3).div_ceil(5).max(1);
    counts
        .into_iter()
        .filter(|(dir, n)| *n >= needed && dir.components().count() > 3)
        .max_by_key(|(dir, n)| (dir.components().count(), *n))
        // The project it is in, when that still exists.
        .map(|(dir, _)| {
            dir.ancestors()
                .find(|a| a.join(".git").exists())
                .unwrap_or(dir)
                .to_string_lossy()
                .into_owned()
        })
        .or_else(|| std::env::var("HOME").ok())
        .unwrap_or_else(|| "/".into())
}

#[cfg(test)]
mod tests;

//! Sessions: transcripts kept in memory and appended to JSONL files.
//!
//! File layout: `<data_dir>/sessions/<session_id>.jsonl`, plus
//! `<session_id>.title` when it was renamed and `<session_id>.queue.json`
//! while messages are queued. The first record is
//! the session header; every later record is one transcript message, or a
//! compaction checkpoint that replaces the transcript before it (the earlier
//! records stay in the file).

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use bone_proto::methods::{QueueMode, QueuedMessage};
use bone_proto::types::{ChatMessage, SessionId, SessionInfo, TurnId};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

const TITLE_CHARS: usize = 80;

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
enum Record {
    Session(Header),
    Message(ChatMessage),
    /// The transcript from here on starts as these messages.
    Compact(Vec<ChatMessage>),
}

#[derive(Serialize, Deserialize)]
struct Header {
    session_id: SessionId,
    cwd: String,
    created_at: u64,
    /// The session it was forked from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent: Option<SessionId>,
}

pub struct Session {
    pub info: SessionInfo,
    pub messages: Vec<ChatMessage>,
    pub active: Option<ActiveTurn>,
    /// The running turn is between model calls, where Lua may add to the
    /// transcript or compact it (never between tool calls and results).
    pub safe_point: bool,
    /// Messages waiting to be sent: steered into the running turn, or turns
    /// of their own after it.
    pub queue: Vec<QueuedMessage>,
    /// The queue waits (after a cancelled turn, or a restart).
    pub queue_paused: bool,
    next_turn: TurnId,
    file: File,
    /// Where the queue is kept.
    queue_path: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct SavedQueue {
    items: Vec<QueuedMessage>,
}

pub struct ActiveTurn {
    pub turn_id: TurnId,
    pub cancel: CancellationToken,
    /// The turn has decided to end; steer messages wait for the next one.
    pub closing: bool,
}

impl Session {
    /// Append to the transcript and the file. A failed write is reported but
    /// the in-memory transcript still advances so the turn can continue.
    pub fn push(&mut self, msg: ChatMessage) -> std::io::Result<()> {
        if self.info.title.is_none() {
            self.info.title = title_of(&msg);
        }
        let written = write_record(&mut self.file, &Record::Message(msg.clone()));
        self.messages.push(msg);
        written
    }

    /// Add to the queue; returns the message's id.
    pub fn enqueue(&mut self, text: String, mode: QueueMode) -> u64 {
        let id = self.queue.iter().map(|q| q.id).max().unwrap_or(0) + 1;
        self.queue.push(QueuedMessage {
            id,
            text,
            mode,
            created_at: now(),
        });
        self.save_queue();
        id
    }

    /// Take the steer messages out of the queue (in order).
    pub fn take_steer(&mut self) -> Vec<QueuedMessage> {
        let (steer, rest) = std::mem::take(&mut self.queue)
            .into_iter()
            .partition(|q| q.mode == QueueMode::Steer);
        self.queue = rest;
        if !steer.is_empty() {
            self.save_queue();
        }
        steer
    }

    pub fn has_steer(&self) -> bool {
        self.queue.iter().any(|q| q.mode == QueueMode::Steer)
    }

    /// Keep the queue on disk (or remove the file when it is empty).
    pub fn save_queue(&self) {
        if self.queue.is_empty() {
            let _ = std::fs::remove_file(&self.queue_path);
            return;
        }
        let saved = SavedQueue {
            items: self.queue.clone(),
        };
        if let Ok(text) = serde_json::to_string(&saved) {
            let tmp = self.queue_path.with_extension("json.tmp");
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(&tmp, &self.queue_path);
            }
        }
    }

    /// Replace the transcript with `messages`, keeping the old records in
    /// the file behind a checkpoint.
    pub fn compact(&mut self, messages: Vec<ChatMessage>) -> std::io::Result<()> {
        let written = write_record(&mut self.file, &Record::Compact(messages.clone()));
        self.messages = messages;
        written
    }

    /// Whether Lua may change the transcript now: no turn, or a turn at a
    /// safe point.
    pub fn writable(&self) -> bool {
        self.active.is_none() || self.safe_point
    }

    pub fn next_turn_id(&mut self) -> TurnId {
        self.next_turn += 1;
        self.next_turn
    }
}

pub type SessionHandle = Arc<Mutex<Session>>;

pub struct SessionStore {
    dir: PathBuf,
    loaded: Mutex<HashMap<SessionId, SessionHandle>>,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("no such session: {0}")]
    NotFound(String),
    #[error("session storage: {0}")]
    Io(#[from] std::io::Error),
    #[error("corrupt session file {0}: {1}")]
    Corrupt(PathBuf, String),
}

impl SessionStore {
    pub fn new(data_dir: &Path) -> Self {
        SessionStore {
            dir: data_dir.join("sessions"),
            loaded: Mutex::new(HashMap::new()),
        }
    }

    pub fn create(&self, cwd: String) -> Result<SessionHandle, SessionError> {
        self.create_with(cwd, None, &[])
    }

    /// A new session, maybe forked from another, starting with `messages`.
    fn create_with(
        &self,
        cwd: String,
        parent: Option<SessionId>,
        messages: &[ChatMessage],
    ) -> Result<SessionHandle, SessionError> {
        std::fs::create_dir_all(&self.dir)?;
        let header = Header {
            session_id: uuid::Uuid::now_v7().to_string(),
            cwd,
            created_at: now(),
            parent,
        };
        let path = self.path(&header.session_id);
        let mut file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(&path)?;
        let info = SessionInfo {
            session_id: header.session_id.clone(),
            cwd: header.cwd.clone(),
            created_at: header.created_at,
            title: messages.iter().find_map(title_of),
            parent: header.parent.clone(),
        };
        write_record(&mut file, &Record::Session(header))?;
        for m in messages {
            write_record(&mut file, &Record::Message(m.clone()))?;
        }
        let users = messages
            .iter()
            .filter(|m| matches!(m, ChatMessage::User { .. }))
            .count();
        let session = Arc::new(Mutex::new(Session {
            queue_path: self.queue_path(&info.session_id),
            info: info.clone(),
            messages: messages.to_vec(),
            active: None,
            safe_point: false,
            queue: Vec::new(),
            queue_paused: false,
            next_turn: users as TurnId,
            file,
        }));
        self.loaded
            .lock()
            .unwrap()
            .insert(info.session_id, session.clone());
        Ok(session)
    }

    /// The session from memory, or loaded from disk.
    pub fn get(&self, id: &str) -> Result<SessionHandle, SessionError> {
        if let Some(s) = self.loaded.lock().unwrap().get(id) {
            return Ok(s.clone());
        }
        // Ids are UUIDs; anything else could escape the sessions directory.
        if uuid::Uuid::parse_str(id).is_err() {
            return Err(SessionError::NotFound(id.to_owned()));
        }
        let path = self.path(id);
        let Loaded {
            mut info,
            messages,
            good_len,
            users,
        } = match read_file(&path, usize::MAX) {
            Ok(r) => r,
            Err(SessionError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(SessionError::NotFound(id.to_owned()));
            }
            Err(e) => return Err(e),
        };
        let file = OpenOptions::new().append(true).open(&path)?;
        // Drop a torn tail so the next append starts on a fresh line.
        if file.metadata()?.len() > good_len {
            file.set_len(good_len)?;
        }
        if let Some(t) = self.saved_title(id) {
            info.title = Some(t);
        }
        // Turn ids count every user message ever written (compacted ones
        // too), so they stay unique across reloads.
        let next_turn = users as TurnId;
        // A queue left from before waits until resumed or added to.
        let queue: Vec<QueuedMessage> = std::fs::read_to_string(self.queue_path(id))
            .ok()
            .and_then(|t| serde_json::from_str::<SavedQueue>(&t).ok())
            .map(|q| q.items)
            .unwrap_or_default();
        let session = Arc::new(Mutex::new(Session {
            queue_path: self.queue_path(id),
            queue_paused: !queue.is_empty(),
            queue,
            info,
            messages,
            active: None,
            safe_point: false,
            next_turn,
            file,
        }));
        // Another caller may have loaded it meanwhile; keep the first.
        let mut loaded = self.loaded.lock().unwrap();
        Ok(loaded.entry(id.to_owned()).or_insert(session).clone())
    }

    /// A copy of a session's transcript (all of it, or what came before
    /// user message `before_turn`, counting from 1) as a new session.
    pub fn fork(&self, id: &str, before_turn: Option<u32>) -> Result<SessionHandle, SessionError> {
        let source = self.get(id)?;
        let (cwd, messages) = {
            let s = source.lock().unwrap();
            let mut end = s.messages.len();
            if let Some(n) = before_turn {
                // Cut at a user message, so no tool call loses its result.
                if let Some((i, _)) = s
                    .messages
                    .iter()
                    .enumerate()
                    .filter(|(_, m)| matches!(m, ChatMessage::User { .. }))
                    .nth((n.max(1) - 1) as usize)
                {
                    end = i;
                }
            }
            (s.info.cwd.clone(), s.messages[..end].to_vec())
        };
        self.create_with(cwd, Some(id.to_owned()), &messages)
    }

    /// Give a session a title that stays (kept next to its file).
    pub fn rename(&self, id: &str, title: &str) -> Result<SessionInfo, SessionError> {
        let session = self.get(id)?;
        std::fs::write(self.title_path(id), title)?;
        let mut s = session.lock().unwrap();
        s.info.title = Some(title.to_owned());
        Ok(s.info.clone())
    }

    /// Remove a session from memory and disk.
    pub fn delete(&self, id: &str) -> Result<(), SessionError> {
        self.get(id)?;
        self.loaded.lock().unwrap().remove(id);
        std::fs::remove_file(self.path(id))?;
        let _ = std::fs::remove_file(self.title_path(id));
        let _ = std::fs::remove_file(self.queue_path(id));
        Ok(())
    }

    fn queue_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.queue.json"))
    }

    fn title_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.title"))
    }

    /// The title a rename gave, if any.
    fn saved_title(&self, id: &str) -> Option<String> {
        std::fs::read_to_string(self.title_path(id))
            .ok()
            .filter(|t| !t.trim().is_empty())
    }

    /// Every session on disk, newest first. Unreadable files are skipped.
    pub fn list(&self) -> Result<Vec<SessionInfo>, SessionError> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let loaded = self.loaded.lock().unwrap().clone();
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "jsonl") {
                continue;
            }
            let id = path.file_stem().unwrap_or_default().to_string_lossy();
            if let Some(s) = loaded.get(id.as_ref()) {
                out.push(s.lock().unwrap().info.clone());
            } else if let Ok(mut l) = read_file(&path, 1) {
                if let Some(t) = self.saved_title(&id) {
                    l.info.title = Some(t);
                }
                out.push(l.info);
            }
        }
        // UUIDv7 ids sort by creation time.
        out.sort_by(|a, b| b.session_id.cmp(&a.session_id));
        Ok(out)
    }

    fn path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.jsonl"))
    }
}

struct Loaded {
    info: SessionInfo,
    messages: Vec<ChatMessage>,
    /// Length of the file up to the last complete record. Shorter than the
    /// file when a crash left a torn final line.
    good_len: u64,
    /// User messages in the whole file, before checkpoints too.
    users: usize,
}

/// Read a session file, stopping after `max_user` user messages (enough to
/// get a title without parsing the whole transcript).
fn read_file(path: &Path, max_user: usize) -> Result<Loaded, SessionError> {
    let corrupt = |why: String| SessionError::Corrupt(path.to_owned(), why);
    let mut reader = BufReader::new(File::open(path)?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let header = match serde_json::from_str(&line) {
        Ok(Record::Session(h)) => h,
        _ => return Err(corrupt("missing session header".into())),
    };
    let mut loaded = Loaded {
        info: SessionInfo {
            session_id: header.session_id,
            cwd: header.cwd,
            created_at: header.created_at,
            title: None,
            parent: header.parent,
        },
        messages: Vec::new(),
        good_len: line.len() as u64,
        users: 0,
    };
    for n in 2.. {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        match serde_json::from_str(&line) {
            Ok(Record::Message(msg)) => {
                if loaded.info.title.is_none() {
                    loaded.info.title = title_of(&msg);
                }
                let is_user = matches!(msg, ChatMessage::User { .. });
                loaded.messages.push(msg);
                loaded.good_len += line.len() as u64;
                if is_user {
                    loaded.users += 1;
                    if loaded.users >= max_user {
                        break;
                    }
                }
            }
            Ok(Record::Compact(messages)) => {
                loaded.messages = messages;
                loaded.good_len += line.len() as u64;
            }
            Ok(Record::Session(_)) => return Err(corrupt(format!("second header at line {n}"))),
            // A torn final write (crash mid-append) should not lose the session.
            Err(_) if !line.ends_with('\n') => break,
            Err(e) => return Err(corrupt(format!("line {n}: {e}"))),
        }
    }
    Ok(loaded)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn write_record(file: &mut File, record: &Record) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(record).map_err(std::io::Error::other)?;
    line.push(b'\n');
    file.write_all(&line)
}

fn title_of(msg: &ChatMessage) -> Option<String> {
    let ChatMessage::User { content } = msg else {
        return None;
    };
    let line = content.lines().find(|l| !l.trim().is_empty())?.trim();
    Some(match line.char_indices().nth(TITLE_CHARS) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_persist_reload_list() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path());
        let a = store.create("/work".into()).unwrap();
        let id = a.lock().unwrap().info.session_id.clone();
        {
            let mut s = a.lock().unwrap();
            s.push(ChatMessage::User {
                content: "\n  fix the bug  \nplease".into(),
            })
            .unwrap();
            s.push(ChatMessage::Assistant {
                content: "done".into(),
                reasoning: String::new(),
                tool_calls: vec![],
            })
            .unwrap();
        }
        let b = store.create("/other".into()).unwrap();
        let b_id = b.lock().unwrap().info.session_id.clone();

        let fresh = SessionStore::new(dir.path());
        let listed = fresh.list().unwrap();
        assert_eq!(
            listed.iter().map(|i| &i.session_id).collect::<Vec<_>>(),
            vec![&b_id, &id]
        );
        assert_eq!(listed[1].title.as_deref(), Some("fix the bug"));

        let loaded = fresh.get(&id).unwrap();
        let s = loaded.lock().unwrap();
        assert_eq!(s.info.cwd, "/work");
        assert_eq!(s.messages.len(), 2);
        assert!(Arc::ptr_eq(&loaded, &fresh.get(&id).unwrap()));
    }

    #[test]
    fn recovers_from_torn_tail() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path());
        let s = store.create("/w".into()).unwrap();
        let id = s.lock().unwrap().info.session_id.clone();
        s.lock()
            .unwrap()
            .push(ChatMessage::User {
                content: "hi".into(),
            })
            .unwrap();
        let path = dir.path().join(format!("sessions/{id}.jsonl"));
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"kind\":\"mess")
            .unwrap();

        let fresh = SessionStore::new(dir.path());
        let s = fresh.get(&id).unwrap();
        s.lock()
            .unwrap()
            .push(ChatMessage::User {
                content: "again".into(),
            })
            .unwrap();
        let again = SessionStore::new(dir.path()).get(&id).unwrap();
        assert_eq!(again.lock().unwrap().messages.len(), 2);
    }

    #[test]
    fn rejects_unknown_and_path_like_ids() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path());
        for id in ["../../etc/passwd", "0190a1b2-0000-7000-8000-000000000000"] {
            assert!(
                matches!(store.get(id), Err(SessionError::NotFound(_))),
                "{id}"
            );
        }
    }
}

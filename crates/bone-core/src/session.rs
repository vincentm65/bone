//! Sessions: transcripts kept in memory and appended to JSONL files.
//!
//! File layout: `<data_dir>/sessions/<session_id>.jsonl`, plus
//! `<session_id>.title` when it was renamed and `<session_id>.queue.json`
//! for its queue and message ID counter. The first record is
//! the session header; every later record is one transcript message, a
//! summary the model gets in place of the transcript's older part (see
//! `compact.rs`), or a checkpoint that replaces the transcript before it
//! (`bone.session.compact`, imports; the earlier records stay in the file).

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use bone_proto::methods::{QueueMode, QueuedMessage};
use bone_proto::types::{ChatMessage, SessionId, SessionInfo, SessionOwner, TurnId};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::index::Index;

const TITLE_CHARS: usize = 80;
/// Sessions kept in memory; past this the least recently used idle ones are
/// dropped, to load again from disk when next asked for.
const MAX_LOADED: usize = 4;

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub(crate) enum Record {
    Session(Header),
    Message(ChatMessage),
    /// The transcript from here on starts as these messages.
    Compact(Vec<ChatMessage>),
    /// From here on the model gets this summary in place of the
    /// transcript's first messages; `null` sends the whole transcript again.
    Summary(Option<Summary>),
    /// What one model call used. Not part of the transcript.
    Usage(UsageRecord),
    /// The provider entry and model this session's turns use from here on.
    Model(Pin),
}

/// A provider entry (`None`: the env-configured one) and model.
pub type Pin = (Option<String>, String);

/// One line of a session file: a record and when it was written.
#[derive(Serialize, Deserialize)]
pub(crate) struct Line {
    #[serde(flatten)]
    pub record: Record,
    /// Unix seconds; missing in files from before it was kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageRecord {
    pub turn_id: TurnId,
    /// The `bone.config.providers` entry, if the call went through one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Input tokens served from the provider's cache, when it says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u64>,
    /// Input context of the last model call, distinct from aggregate consumption.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u64>,
    /// `None` for a turn; `"lua"` or `"client"` for a call made outside one
    /// (then `turn_id` is 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// A summary standing in for the transcript's first `through` messages in
/// what the model is sent. The transcript itself keeps them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub through: usize,
    pub text: String,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Header {
    pub session_id: SessionId,
    pub cwd: String,
    pub created_at: u64,
    /// The session it was forked from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<SessionId>,
    /// The session that started this one (a subagent's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<SessionOwner>,
}

pub struct Session {
    pub info: SessionInfo,
    pub messages: Vec<ChatMessage>,
    /// What the model gets in place of the transcript's older part.
    pub summary: Option<Summary>,
    /// Characters per token in this session's last model call, to estimate
    /// sizes; `None` until a reply reports its usage.
    pub chars_per_token: Option<f64>,
    /// Replay mode used by the estimate and its learned ratio (not persisted).
    pub replays_reasoning: bool,
    /// Characters of the tool definitions sent with the last model call:
    /// the provider counts them in its usage, so estimates must too.
    pub tool_chars: usize,
    /// A compaction is writing its summary.
    pub compacting: bool,
    /// Counts `bone.session.compact` rewrites, which invalidate positions
    /// in the transcript.
    pub generation: u64,
    pub active: Option<ActiveTurn>,
    /// The running turn is between model calls, where Lua may add to the
    /// transcript or compact it (never between tool calls and results).
    pub safe_point: bool,
    /// Messages waiting to be sent: steered into the running turn, or turns
    /// of their own after it.
    pub queue: Vec<QueuedMessage>,
    /// The queue waits (after a cancelled turn, or a restart).
    pub queue_paused: bool,
    /// The model it is locked to; `None` until its first turn or change.
    pub model: Option<Pin>,
    next_turn: TurnId,
    next_queue: u64,
    file: File,
    path: PathBuf,
    index: Option<Arc<Index>>,
    /// Where the queue is kept.
    queue_path: PathBuf,
}

#[derive(Default, Serialize, Deserialize)]
struct SavedQueue {
    items: Vec<QueuedMessage>,
    #[serde(default)]
    next_id: u64,
}

pub struct ActiveTurn {
    pub turn_id: TurnId,
    pub cancel: CancellationToken,
    /// The turn has decided to end; steer messages wait for the next one.
    pub closing: bool,
}

impl Session {
    /// A ratio learned without reasoning cannot estimate replayed reasoning,
    /// or vice versa. Keep the mode even when no usage has been reported.
    pub(crate) fn set_replays_reasoning(&mut self, replays_reasoning: bool) {
        if self.replays_reasoning != replays_reasoning {
            self.chars_per_token = None;
            self.replays_reasoning = replays_reasoning;
        }
    }
    /// Append to the transcript and the file. A failed write is reported but
    /// the in-memory transcript still advances so the turn can continue.
    pub fn push(&mut self, msg: ChatMessage) -> std::io::Result<()> {
        if self.info.title.is_none() {
            self.info.title = title_of(&msg);
        }
        let written = write_record(&mut self.file, Record::Message(msg.clone()));
        self.messages.push(msg);
        self.reindex();
        written
    }

    /// Keep what a model call used (in the file, not the transcript).
    pub fn usage(&mut self, usage: UsageRecord) -> std::io::Result<()> {
        let written = write_record(&mut self.file, Record::Usage(usage));
        self.reindex();
        written
    }

    /// Lock the session to `model` (kept in the file).
    pub fn pin(&mut self, model: Pin) -> std::io::Result<()> {
        self.model = Some(model.clone());
        write_record(&mut self.file, Record::Model(model))
    }

    /// Bring the index up to date with the file.
    fn reindex(&self) {
        if let Some(index) = &self.index
            && let Err(e) = index.sync(&self.info.session_id, &self.path, None)
        {
            eprintln!("bone: session index: {e}");
        }
    }

    /// Add to the queue; returns the message's id.
    pub fn enqueue(&mut self, text: String, mode: QueueMode) -> u64 {
        self.enqueue_with_images(text, mode, Vec::new())
    }

    pub fn enqueue_with_images(
        &mut self,
        text: String,
        mode: QueueMode,
        images: Vec<bone_proto::types::ImageAttachment>,
    ) -> u64 {
        self.next_queue += 1;
        let id = self.next_queue;
        self.queue.push(QueuedMessage {
            id,
            text,
            mode,
            created_at: now(),
            images,
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

    /// Keep the queue and its ID counter on disk, even when empty.
    pub fn save_queue(&self) {
        let saved = SavedQueue {
            items: self.queue.clone(),
            next_id: self.next_queue,
        };
        let tmp = self.queue_path.with_extension("json.tmp");
        if let Ok(text) = serde_json::to_string(&saved)
            && std::fs::write(&tmp, text).is_ok()
        {
            let _ = std::fs::rename(&tmp, &self.queue_path);
        }
    }

    /// Replace the transcript with `messages`, keeping the old records in
    /// the file behind a checkpoint. A summary of the old transcript goes.
    pub fn compact(&mut self, messages: Vec<ChatMessage>) -> std::io::Result<()> {
        let written = write_record(&mut self.file, Record::Compact(messages.clone()));
        self.messages = messages;
        self.summary = None;
        self.generation += 1;
        self.reindex();
        written
    }

    /// Set (or clear) the summary the model gets in place of the
    /// transcript's older part.
    pub fn summarize(&mut self, summary: Option<Summary>) -> std::io::Result<()> {
        let written = write_record(&mut self.file, Record::Summary(summary.clone()));
        self.summary = summary;
        written
    }

    /// The transcript as the model is sent it: the summary, then what it
    /// does not cover.
    pub fn context(&self) -> Vec<ChatMessage> {
        let messages = match &self.summary {
            Some(sum) if sum.through <= self.messages.len() => {
                let mut out = vec![crate::compact::summary_message(&sum.text)];
                out.extend(self.messages[sum.through..].iter().cloned());
                out
            }
            _ => self.messages.clone(),
        };
        crate::import::repair(messages)
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
    /// With when each was last asked for.
    loaded: Mutex<HashMap<SessionId, (SessionHandle, u64)>>,
    tick: AtomicU64,
    /// What `list` read from files not loaded, by their modification time.
    listed: Mutex<HashMap<SessionId, (SystemTime, SessionInfo)>>,
    /// `None` when the index could not be opened; sessions work without.
    index: Option<Arc<Index>>,
    /// Usage of model calls outside any session.
    usage_log: PathBuf,
    usage_lock: Mutex<()>,
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
        let index = match Index::open(&data_dir.join("index.db")) {
            Ok(i) => Some(Arc::new(i)),
            Err(e) => {
                eprintln!("bone: session index unavailable: {e}");
                None
            }
        };
        SessionStore {
            dir: data_dir.join("sessions"),
            loaded: Mutex::new(HashMap::new()),
            tick: Default::default(),
            listed: Mutex::new(HashMap::new()),
            index,
            usage_log: data_dir.join("usage.jsonl"),
            usage_lock: Mutex::new(()),
        }
    }

    pub fn create(&self, cwd: String) -> Result<SessionHandle, SessionError> {
        self.create_with(cwd, None, None, &[], None)
    }

    /// A new session started on behalf of another (a subagent).
    pub fn create_owned(
        &self,
        cwd: String,
        owner: SessionOwner,
    ) -> Result<SessionHandle, SessionError> {
        self.create_with(cwd, None, Some(owner), &[], None)
    }

    /// A new session, maybe forked from another, starting with `messages`
    /// (and the summary standing in for their first part).
    fn create_with(
        &self,
        cwd: String,
        parent: Option<SessionId>,
        owner: Option<SessionOwner>,
        messages: &[ChatMessage],
        summary: Option<Summary>,
    ) -> Result<SessionHandle, SessionError> {
        std::fs::create_dir_all(&self.dir)?;
        let header = Header {
            session_id: uuid::Uuid::now_v7().to_string(),
            cwd,
            created_at: now(),
            parent,
            owner,
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
            owner: header.owner.clone(),
        };
        write_record(&mut file, Record::Session(header))?;
        for m in messages {
            write_record(&mut file, Record::Message(m.clone()))?;
        }
        if summary.is_some() {
            write_record(&mut file, Record::Summary(summary.clone()))?;
        }
        let users = messages
            .iter()
            .filter(|m| matches!(m, ChatMessage::User { .. }))
            .count();
        let session = Arc::new(Mutex::new(Session {
            queue_path: self.queue_path(&info.session_id),
            info: info.clone(),
            messages: messages.to_vec(),
            summary,
            chars_per_token: None,
            replays_reasoning: false,
            tool_chars: 0,
            compacting: false,
            generation: 0,
            active: None,
            safe_point: false,
            queue: Vec::new(),
            queue_paused: false,
            model: None,
            next_turn: users as TurnId,
            next_queue: 0,
            file,
            path,
            index: self.index.clone(),
        }));
        session.lock().unwrap().reindex();
        Ok(self.keep(&info.session_id, session))
    }

    /// The session from memory, or loaded from disk.
    pub fn get(&self, id: &str) -> Result<SessionHandle, SessionError> {
        if let Some((s, used)) = self.loaded.lock().unwrap().get_mut(id) {
            *used = self.tick.fetch_add(1, Ordering::Relaxed);
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
            summary,
            good_len,
            users,
            model,
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
        let saved = std::fs::read_to_string(self.queue_path(id))
            .ok()
            .and_then(|t| serde_json::from_str::<SavedQueue>(&t).ok())
            .unwrap_or_default();
        let next_queue = saved
            .next_id
            .max(saved.items.iter().map(|q| q.id).max().unwrap_or(0));
        let queue = saved.items;
        let session = Arc::new(Mutex::new(Session {
            queue_path: self.queue_path(id),
            queue_paused: !queue.is_empty(),
            queue,
            info,
            messages,
            summary,
            chars_per_token: None,
            replays_reasoning: false,
            tool_chars: 0,
            compacting: false,
            generation: 0,
            active: None,
            safe_point: false,
            model,
            next_turn,
            next_queue,
            file,
            path,
            index: self.index.clone(),
        }));
        Ok(self.keep(id, session))
    }

    /// Hold `session` in memory (or the one another caller loaded meanwhile)
    /// and drop the least recently used idle ones past `MAX_LOADED`: those
    /// nothing else holds, with no turn, queue or compaction.
    fn keep(&self, id: &str, session: SessionHandle) -> SessionHandle {
        let mut loaded = self.loaded.lock().unwrap();
        let used = self.tick.fetch_add(1, Ordering::Relaxed);
        let session = loaded
            .entry(id.to_owned())
            .or_insert((session, used))
            .0
            .clone();
        while loaded.len() > MAX_LOADED {
            let idle = (loaded.iter())
                .filter(|(_, (s, _))| {
                    Arc::strong_count(s) == 1
                        && s.try_lock().is_ok_and(|s| {
                            s.active.is_none() && s.queue.is_empty() && !s.compacting
                        })
                })
                .min_by_key(|(_, (_, used))| *used)
                .map(|(id, _)| id.clone());
            let Some(idle) = idle else { break };
            loaded.remove(&idle);
        }
        session
    }

    /// A copy of a session's transcript (all of it, or what came before
    /// user message `before_turn`, counting from 1) as a new session.
    pub fn fork(&self, id: &str, before_turn: Option<u32>) -> Result<SessionHandle, SessionError> {
        let source = self.get(id)?;
        let (cwd, messages, summary) = {
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
            // The summary comes along when it covers only what is copied.
            let summary = s.summary.clone().filter(|sum| sum.through <= end);
            (s.info.cwd.clone(), s.messages[..end].to_vec(), summary)
        };
        let fork = self.create_with(cwd, Some(id.to_owned()), None, &messages, summary)?;
        let model = source.lock().unwrap().model.clone();
        model.map_or(Ok(()), |m| fork.lock().unwrap().pin(m))?;
        Ok(fork)
    }

    /// Give a session a title that stays (kept next to its file).
    pub fn rename(&self, id: &str, title: &str) -> Result<SessionInfo, SessionError> {
        let session = self.get(id)?;
        std::fs::write(self.title_path(id), title)?;
        if let Some(index) = &self.index {
            let _ = index.rename(id, title);
        }
        let mut s = session.lock().unwrap();
        s.info.title = Some(title.to_owned());
        Ok(s.info.clone())
    }

    /// Remove a session from memory and disk.
    pub fn delete(&self, id: &str) -> Result<(), SessionError> {
        self.get(id)?;
        self.loaded.lock().unwrap().remove(id);
        self.listed.lock().unwrap().remove(id);
        std::fs::remove_file(self.path(id))?;
        let _ = std::fs::remove_file(self.title_path(id));
        let _ = std::fs::remove_file(self.queue_path(id));
        if let Some(index) = &self.index {
            let _ = index.remove(id);
        }
        Ok(())
    }

    /// Keep what a model call outside a turn used: in its session's file
    /// (at the turn running, if any), or in `usage.jsonl` without one.
    /// Failures only cost the record.
    pub fn record_usage(&self, session: Option<&str>, mut usage: UsageRecord) {
        if let Some(s) = session.and_then(|id| self.get(id).ok()) {
            let mut s = s.lock().unwrap();
            usage.turn_id = s.active.as_ref().map_or(0, |a| a.turn_id);
            let _ = s.usage(usage);
            return;
        }
        let _guard = self.usage_lock.lock().unwrap();
        let written = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.usage_log)
            .and_then(|mut f| write_record(&mut f, Record::Usage(usage)));
        if written.is_ok()
            && let Some(index) = &self.index
            && let Err(e) = index.sync("", &self.usage_log, None)
        {
            eprintln!("bone: session index: {e}");
        }
    }

    pub fn index(&self) -> Option<&Arc<Index>> {
        self.index.as_ref()
    }

    /// Bring the index up to date with every session file (new, grown,
    /// rewritten or gone). Cheap for files already indexed.
    pub fn catch_up(&self) {
        let Some(index) = &self.index else {
            return;
        };
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        let mut ids = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "jsonl") {
                continue;
            }
            let id = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let title = self.saved_title(&id);
            if let Err(e) = index.sync(&id, &path, title.as_deref()) {
                eprintln!("bone: session index: {id}: {e}");
            }
            ids.push(id);
        }
        let _ = index.retain(&ids);
        if self.usage_log.exists() {
            let _guard = self.usage_lock.lock().unwrap();
            let _ = index.sync("", &self.usage_log, None);
        }
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

    /// Running sessions are always held in memory; no disk reads are needed.
    pub fn active(&self) -> Vec<SessionId> {
        let sessions: Vec<_> = self
            .loaded
            .lock()
            .unwrap()
            .iter()
            .map(|(id, (session, _))| (id.clone(), session.clone()))
            .collect();
        sessions
            .into_iter()
            .filter_map(|(id, session)| session.lock().unwrap().active.is_some().then_some(id))
            .collect()
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
            if let Some((s, _)) = loaded.get(id.as_ref()) {
                out.push(s.lock().unwrap().info.clone());
                continue;
            }
            // Only files changed since the last list are read again.
            let Ok(mtime) = entry.metadata().and_then(|m| m.modified()) else {
                continue;
            };
            let mut listed = self.listed.lock().unwrap();
            let cached = listed.get(id.as_ref()).filter(|(t, _)| *t == mtime);
            let Some(mut info) = (cached.map(|(_, i)| i.clone()))
                .or_else(|| read_file(&path, 1).ok().map(|l| l.info))
            else {
                continue;
            };
            listed.insert(id.to_string(), (mtime, info.clone()));
            if let Some(t) = self.saved_title(&id) {
                info.title = Some(t);
            }
            out.push(info);
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
    summary: Option<Summary>,
    /// Length of the file up to the last complete record. Shorter than the
    /// file when a crash left a torn final line.
    good_len: u64,
    /// User messages in the whole file, before checkpoints too.
    users: usize,
    model: Option<Pin>,
}

/// Read a session file, stopping after `max_user` user messages (enough to
/// get a title without parsing the whole transcript).
fn read_file(path: &Path, max_user: usize) -> Result<Loaded, SessionError> {
    let corrupt = |why: String| SessionError::Corrupt(path.to_owned(), why);
    let mut reader = BufReader::new(File::open(path)?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    // Timestamps are only needed by the index. Reading Record directly avoids
    // buffering every message's JSON through Line's flattened fields.
    let header = match serde_json::from_str::<Record>(&line) {
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
            owner: header.owner,
        },
        messages: Vec::new(),
        summary: None,
        good_len: line.len() as u64,
        users: 0,
        model: None,
    };
    for n in 2.. {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        match serde_json::from_str::<Record>(&line) {
            Ok(Record::Usage(_)) => loaded.good_len += line.len() as u64,
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
                loaded.summary = None;
                loaded.good_len += line.len() as u64;
            }
            Ok(Record::Summary(summary)) => {
                loaded.summary = summary;
                loaded.good_len += line.len() as u64;
            }
            Ok(Record::Model(model)) => {
                loaded.model = Some(model);
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

pub(crate) fn write_record(file: &mut File, record: Record) -> std::io::Result<()> {
    let line = Line {
        record,
        at: Some(now()),
    };
    let mut line = serde_json::to_vec(&line).map_err(std::io::Error::other)?;
    line.push(b'\n');
    file.write_all(&line)
}

pub(crate) fn title_of(msg: &ChatMessage) -> Option<String> {
    let ChatMessage::User { content, images } = msg else {
        return None;
    };
    let line = content
        .lines()
        .find(|l| !l.trim().is_empty())
        .map(str::trim)
        .or_else(|| images.first().map(|i| i.name.as_str()))?;
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
                images: Vec::new(),
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
    fn summaries_survive_reloads_and_follow_forks() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path());
        let a = store.create("/work".into()).unwrap();
        let id = a.lock().unwrap().info.session_id.clone();
        let user = |t: &str| ChatMessage::User {
            content: t.into(),
            images: Vec::new(),
        };
        {
            let mut s = a.lock().unwrap();
            for t in ["q1", "q2", "q3"] {
                s.push(user(t)).unwrap();
            }
            s.summarize(Some(Summary {
                through: 2,
                text: "S".into(),
            }))
            .unwrap();
            assert_eq!(
                s.context(),
                vec![crate::compact::summary_message("S"), user("q3")]
            );
        }

        // From disk: the whole transcript, and the summary for the model.
        let fresh = SessionStore::new(dir.path());
        let loaded = fresh.get(&id).unwrap();
        assert_eq!(loaded.lock().unwrap().messages.len(), 3);
        assert_eq!(loaded.lock().unwrap().context().len(), 2);

        // A fork keeps the summary only when it covers part of the copy.
        let whole = fresh.fork(&id, None).unwrap();
        assert_eq!(whole.lock().unwrap().context().len(), 2);
        let early = fresh.fork(&id, Some(2)).unwrap();
        assert_eq!(early.lock().unwrap().summary, None);

        // Clearing sends everything again; so does replacing the transcript.
        let mut s = loaded.lock().unwrap();
        s.summarize(None).unwrap();
        assert_eq!(s.context().len(), 3);
        s.summarize(Some(Summary {
            through: 1,
            text: "T".into(),
        }))
        .unwrap();
        s.compact(vec![user("restart")]).unwrap();
        assert_eq!(s.context(), vec![user("restart")]);
        drop(s);
        let again = SessionStore::new(dir.path()).get(&id).unwrap();
        assert_eq!(again.lock().unwrap().summary, None);
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
                images: Vec::new(),
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
                images: Vec::new(),
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

    fn rows(store: &SessionStore, sql: &str) -> Vec<Vec<serde_json::Value>> {
        let out = store
            .index()
            .unwrap()
            .query(sql, &serde_json::Value::Null)
            .unwrap();
        serde_json::from_value(out["rows"].clone()).unwrap()
    }

    #[test]
    fn the_index_follows_the_files_and_rebuilds_from_them() {
        use bone_proto::types::ToolCall;
        use serde_json::json;
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path());
        let s = store.create("/w".into()).unwrap();
        let id = s.lock().unwrap().info.session_id.clone();
        {
            let mut s = s.lock().unwrap();
            s.push(ChatMessage::User {
                content: "find the walrus".into(),
                images: Vec::new(),
            })
            .unwrap();
            s.push(ChatMessage::Assistant {
                content: String::new(),
                reasoning: String::new(),
                tool_calls: vec![
                    ToolCall {
                        id: "a".into(),
                        name: "shell".into(),
                        arguments: "{}".into(),
                    },
                    ToolCall {
                        id: "b".into(),
                        name: "edit_file".into(),
                        arguments: "{}".into(),
                    },
                ],
            })
            .unwrap();
            s.usage(UsageRecord {
                turn_id: 1,
                provider: Some("main".into()),
                model: "m1".into(),
                input_tokens: 100,
                output_tokens: 7,
                context_tokens: None,
                cached_tokens: None,
                source: None,
            })
            .unwrap();
            for (call, err) in [("a", false), ("b", true)] {
                s.push(ChatMessage::Tool {
                    images: Vec::new(),
                    call_id: call.into(),
                    content: "out".into(),
                    is_error: err,
                })
                .unwrap();
            }
            s.push(ChatMessage::Assistant {
                content: "the walrus is in the attic".into(),
                reasoning: String::new(),
                tool_calls: vec![],
            })
            .unwrap();
        }
        store.rename(&id, "Walrus hunt").unwrap();
        let summary = "SELECT coalesce(renamed, title), messages FROM sessions";
        let tools = "SELECT name, is_error FROM tool_calls ORDER BY name";
        let usage = "SELECT model, input_tokens, output_tokens FROM usage";
        let search = "SELECT role, seq FROM search WHERE search MATCH 'walrus' ORDER BY seq";
        let check = |store: &SessionStore| {
            assert_eq!(rows(store, summary), [[json!("Walrus hunt"), json!(5)]]);
            assert_eq!(
                rows(store, tools),
                [[json!("edit_file"), json!(1)], [json!("shell"), json!(0)]]
            );
            assert_eq!(rows(store, usage), [[json!("m1"), json!(100), json!(7)]]);
            assert_eq!(
                rows(store, search),
                [[json!("user"), json!(1)], [json!("assistant"), json!(5)]]
            );
        };
        check(&store);

        // The index is derived: without it, catching up rebuilds it.
        drop(store);
        drop(s);
        for f in ["index.db", "index.db-wal", "index.db-shm"] {
            let _ = std::fs::remove_file(dir.path().join(f));
        }
        let store = SessionStore::new(dir.path());
        store.catch_up();
        check(&store);

        // A line still being written waits; a deleted session goes.
        let path = store.path(&id);
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(br#"{"kind":"message","data":{"role":"user","content":"x"}"#)
            .unwrap();
        store.catch_up();
        assert_eq!(rows(&store, "SELECT messages FROM sessions"), [[json!(5)]]);
        std::fs::remove_file(&path).unwrap();
        store.catch_up();
        assert!(rows(&store, "SELECT * FROM sessions").is_empty());
        assert!(rows(&store, "SELECT * FROM usage").is_empty());
    }

    #[test]
    fn queries_are_read_only_and_take_parameters() {
        use serde_json::json;
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path());
        store.create("/a".into()).unwrap();
        store.create("/b".into()).unwrap();
        let index = store.index().unwrap();
        let err = index
            .query("DELETE FROM sessions", &serde_json::Value::Null)
            .unwrap_err();
        assert!(err.contains("readonly"), "{err}");
        let out = index
            .query("SELECT cwd FROM sessions WHERE cwd = ?1", &json!(["/b"]))
            .unwrap();
        assert_eq!(out["rows"], json!([["/b"]]));
        let out = index
            .query(
                "SELECT count(*) AS n FROM sessions WHERE cwd != :cwd",
                &json!({"cwd": "/b"}),
            )
            .unwrap();
        assert_eq!(
            out,
            json!({"columns": ["n"], "rows": [[1]], "truncated": false})
        );
        assert!(index.query("SELEC nope", &serde_json::Value::Null).is_err());
    }

    #[test]
    fn files_from_before_timestamps_still_load() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path());
        let id = uuid::Uuid::now_v7().to_string();
        std::fs::create_dir_all(dir.path().join("sessions")).unwrap();
        std::fs::write(
            store.path(&id),
            format!(
                "{{\"kind\":\"session\",\"data\":{{\"session_id\":\"{id}\",\"cwd\":\"/w\",\"created_at\":5}}}}\n\
                 {{\"kind\":\"message\",\"data\":{{\"role\":\"user\",\"content\":\"old\"}}}}\n"
            ),
        )
        .unwrap();
        let s = store.get(&id).unwrap();
        assert_eq!(s.lock().unwrap().messages.len(), 1);
        store.catch_up();
        let out = rows(&store, "SELECT updated_at, messages FROM sessions");
        assert_eq!(out, [[serde_json::json!(5), serde_json::json!(1)]]);
    }

    #[test]
    fn idle_sessions_past_the_cap_are_dropped_and_load_again() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path());
        let held = store.create("/held".into()).unwrap();
        let first = store.create("/first".into()).unwrap();
        let first_id = first.lock().unwrap().info.session_id.clone();
        first
            .lock()
            .unwrap()
            .push(ChatMessage::User {
                content: "kept on disk".into(),
                images: Vec::new(),
            })
            .unwrap();
        drop(first);
        for _ in 0..MAX_LOADED + 4 {
            store.create("/more".into()).unwrap();
        }
        let loaded = store.loaded.lock().unwrap();
        assert_eq!(loaded.len(), MAX_LOADED);
        // Held elsewhere stays; the least recently used idle one went.
        assert!(loaded.contains_key(&held.lock().unwrap().info.session_id));
        assert!(!loaded.contains_key(&first_id));
        drop(loaded);
        let again = store.get(&first_id).unwrap();
        assert_eq!(again.lock().unwrap().messages.len(), 1);
    }

    #[test]
    fn list_reads_a_file_again_once_it_changes() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path());
        let id = store
            .create("/w".into())
            .unwrap()
            .lock()
            .unwrap()
            .info
            .session_id
            .clone();
        // Not loaded, so `list` reads (and caches) the file.
        store.loaded.lock().unwrap().clear();
        assert_eq!(store.list().unwrap()[0].title, None);
        let mut f = OpenOptions::new()
            .append(true)
            .open(store.path(&id))
            .unwrap();
        write_record(
            &mut f,
            Record::Message(ChatMessage::User {
                content: "a title".into(),
                images: Vec::new(),
            }),
        )
        .unwrap();
        let later = SystemTime::now() + std::time::Duration::from_secs(5);
        f.set_modified(later).unwrap();
        assert_eq!(store.list().unwrap()[0].title.as_deref(), Some("a title"));
    }
}

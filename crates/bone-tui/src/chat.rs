//! Chat buffers: a session transcript built from protocol events.
//!
//! This module only holds data and a render cache. A transcript is a list of
//! *items* (user, reasoning, assistant, tool, notice); how each one looks is
//! decided elsewhere (Lua views, see `ui.rs`). Rendered lines are cached per
//! item and redone only when the item, the width or the views change, when
//! chat data the view read has changed (see [`Dep`]), or when the view asked
//! to be drawn again later.

use std::collections::HashMap;
use std::time::Instant;

use crate::data::Dep;

use bone_proto::methods::{
    MessageCompletedParams, MessageDeltaParams, QueueMode, QueuedMessage, ToolFinishedParams,
    ToolOutputParams, ToolStartedParams, TurnFinishedParams,
};
use bone_proto::types::{
    ChatMessage, DeltaKind, SessionInfo, ToolCall, TurnId, TurnOutcome, Usage,
};
use ratatui::text::Line;
use serde_json::{Value, json};

use crate::text::{clean, sanitize, truncate, wrap};

#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    User(String),
    Assistant {
        text: String,
        reasoning: String,
        streaming: bool,
    },
    Tool {
        call: ToolCall,
        output: Option<(String, bool)>,
    },
    Notice {
        text: String,
        error: bool,
    },
}

/// What kind of item: an assistant entry yields a reasoning item and/or an
/// assistant (text) item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Part {
    User,
    Reasoning,
    Assistant,
    Tool,
    Notice,
    /// A message waiting in the session's queue (after the transcript).
    Queued,
    /// An item Lua added (`bone.chat.add`); its kind is its own.
    Lua,
}

/// `Item::entry` of Lua items starts here (their slot added), and of queued
/// messages here (their position added): clear of the transcript's ids.
const LUA_BASE: usize = 1 << 40;
const QUEUE_BASE: usize = 1 << 41;

/// An item Lua put in the chat. It sits after the transcript entries that
/// existed when it was added and is drawn by `bone.ui.views[kind]`. It is
/// the TUI's alone: the core never sees it, and reloading the chat drops it.
#[derive(Debug, Clone)]
pub struct LuaItem {
    pub id: String,
    pub kind: String,
    /// Its fields, as given to `bone.chat.add`/`update`.
    pub data: serde_json::Map<String, Value>,
    /// Shown after the transcript entry with this id (or the latest one
    /// before it, if it is gone), or first (`None`).
    after: Option<usize>,
    rev: u64,
}

impl Part {
    pub fn name(self) -> &'static str {
        match self {
            Part::User => "user",
            Part::Reasoning => "reasoning",
            Part::Assistant => "assistant",
            Part::Tool => "tool",
            Part::Notice => "notice",
            Part::Queued => "queued",
            Part::Lua => "lua",
        }
    }
}

/// One renderable piece of the transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Item {
    pub entry: usize,
    pub part: Part,
}

#[derive(Debug, Clone, Copy)]
pub struct RunningTurn {
    pub turn_id: TurnId,
    pub started: Instant,
}

/// Everything a cached render depends on besides the item's own data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderKey {
    pub width: usize,
    pub rev: u64,
    /// Bumped when views, options or colors change.
    pub generation: (u64, u64),
    pub prev: Option<Part>,
}

#[derive(Default)]
pub struct ChatBuffer {
    /// `None` until the first message creates the session.
    pub session: Option<SessionInfo>,
    pub entries: Vec<Entry>,
    pub turn: Option<RunningTurn>,
    /// A prompt was sent and the turn has not started yet.
    pub starting: bool,
    /// The session's queue, as the core last said (`queue/changed`).
    pub queue: Vec<QueuedMessage>,
    /// The queue waits (after a cancel or a restart) until resumed.
    pub queue_paused: bool,
    /// Bumped when the queue changes; the cache key of queued items.
    queue_rev: u64,
    /// How finished turns ended, by the id of their user message's entry.
    /// Only turns seen finishing here; loaded history has none.
    pub outcomes: HashMap<usize, TurnOutcome>,
    /// Each entry's id: stable, never reused, increasing along `entries`
    /// (entries are only pushed, popped or all cleared). Items, the render
    /// cache and everything else about an entry go by its id.
    ids: Vec<usize>,
    next_id: usize,
    /// Bumped on every change to an entry; part of each cache key.
    revs: Vec<u64>,
    next_rev: u64,
    cache: HashMap<Item, Cached>,
    /// Items Lua added, by slot (`None` once removed).
    lua_items: Vec<Option<LuaItem>>,
    /// Timing and live output of tool calls, by call id.
    tool_meta: HashMap<String, ToolMeta>,
    /// Token usage of a model message, by the id of its first entry.
    message_usage: HashMap<usize, Usage>,
}

/// Live output keeps at most this many bytes, the latest.
const LIVE_MAX: usize = 64 * 1024;

#[derive(Debug, Default, Clone)]
struct ToolMeta {
    started_at: Option<u64>,
    duration_ms: Option<u64>,
    /// What `tool/output` sent while it ran.
    live: String,
}

/// One item's rendered lines and what they were made from.
struct Cached {
    key: RenderKey,
    lines: Vec<Line<'static>>,
    /// Chat data the view read while drawing them.
    deps: Vec<Dep>,
    /// Drawn again from this moment (`bone.chat.refresh_in`).
    expires: Option<Instant>,
    /// The data generation `deps` were last found current at.
    checked: u64,
}

impl ChatBuffer {
    pub fn new(session: Option<SessionInfo>) -> Self {
        ChatBuffer {
            session,
            ..Default::default()
        }
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session.as_ref().map(|s| s.session_id.as_str())
    }

    pub fn title(&self) -> String {
        match &self.session {
            None => "[new session]".into(),
            Some(s) => s.title.clone().unwrap_or_else(|| "[untitled]".into()),
        }
    }

    fn push(&mut self, e: Entry) {
        self.entries.push(e);
        self.next_id += 1;
        self.ids.push(self.next_id);
        self.next_rev += 1;
        self.revs.push(self.next_rev);
    }

    fn pop(&mut self) {
        self.entries.pop();
        self.revs.pop();
        if let Some(id) = self.ids.pop() {
            self.cache.retain(|item, _| item.entry != id);
        }
    }

    /// Where the entry with id `id` is in `entries`.
    fn position(&self, id: usize) -> Option<usize> {
        self.ids.binary_search(&id).ok()
    }

    fn touch(&mut self, i: usize) {
        self.next_rev += 1;
        self.revs[i] = self.next_rev;
    }

    // ---- building the transcript ---------------------------------------------

    /// Replace everything with a loaded transcript.
    pub fn load(
        &mut self,
        info: SessionInfo,
        messages: &[ChatMessage],
        active_turn: Option<TurnId>,
    ) {
        self.session = Some(info);
        self.entries.clear();
        self.ids.clear();
        self.revs.clear();
        self.cache.clear();
        self.lua_items.clear();
        self.tool_meta.clear();
        self.message_usage.clear();
        self.outcomes.clear();
        for m in messages {
            match m {
                ChatMessage::System { .. } => {}
                ChatMessage::User { content } => self.push(Entry::User(content.clone())),
                ChatMessage::Assistant { .. } => self.add_assistant_message(m),
                ChatMessage::Tool {
                    call_id,
                    content,
                    is_error,
                } => self.set_tool_output(call_id, content.clone(), *is_error),
            }
        }
        self.turn = active_turn.map(|turn_id| RunningTurn {
            turn_id,
            started: Instant::now(),
        });
    }

    pub fn turn_started(&mut self, turn_id: TurnId, text: &str) {
        if self.session.as_ref().is_some_and(|s| s.title.is_none()) {
            let title = text
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .trim();
            self.session.as_mut().unwrap().title = Some(truncate(title, 80));
        }
        self.push(Entry::User(text.to_owned()));
        self.turn = Some(RunningTurn {
            turn_id,
            started: Instant::now(),
        });
    }

    pub fn delta(&mut self, d: &MessageDeltaParams) {
        let i = match self.entries.last() {
            Some(Entry::Assistant {
                streaming: true, ..
            }) => self.entries.len() - 1,
            _ => {
                self.push(Entry::Assistant {
                    text: String::new(),
                    reasoning: String::new(),
                    streaming: true,
                });
                self.entries.len() - 1
            }
        };
        if let Entry::Assistant {
            text, reasoning, ..
        } = &mut self.entries[i]
        {
            match d.kind {
                DeltaKind::Text => text.push_str(&d.text),
                DeltaKind::Reasoning => reasoning.push_str(&d.text),
            }
        }
        self.touch(i);
    }

    /// The final message replaces whatever streamed (deltas may have been
    /// missed, e.g. when attaching mid-turn).
    pub fn message_completed(&mut self, m: &MessageCompletedParams) {
        if let Some(Entry::Assistant {
            streaming: true, ..
        }) = self.entries.last()
        {
            self.pop();
        }
        let first = self.entries.len();
        self.add_assistant_message(&m.message);
        if let Some(u) = m.usage
            && let Some(&id) = self.ids.get(first)
        {
            self.message_usage.insert(id, u);
        }
    }

    fn add_assistant_message(&mut self, m: &ChatMessage) {
        let ChatMessage::Assistant {
            content,
            reasoning,
            tool_calls,
        } = m
        else {
            return;
        };
        if !content.trim().is_empty() || !reasoning.trim().is_empty() {
            self.push(Entry::Assistant {
                text: content.clone(),
                reasoning: reasoning.clone(),
                streaming: false,
            });
        }
        for call in tool_calls {
            self.push(Entry::Tool {
                call: call.clone(),
                output: None,
            });
        }
    }

    pub fn tool_started(&mut self, t: &ToolStartedParams) {
        if t.started_at.is_some() {
            self.tool_meta
                .entry(t.call.id.clone())
                .or_default()
                .started_at = t.started_at;
            self.touch_tool(&t.call.id);
        }
        let known = self
            .entries
            .iter()
            .any(|e| matches!(e, Entry::Tool { call, .. } if call.id == t.call.id));
        if !known {
            self.push(Entry::Tool {
                call: t.call.clone(),
                output: None,
            });
        }
    }

    /// Output a running tool sent (`tool/output`).
    pub fn tool_output(&mut self, t: &ToolOutputParams) {
        let live = &mut self.tool_meta.entry(t.call_id.clone()).or_default().live;
        live.push_str(&t.text);
        if live.len() > LIVE_MAX {
            let mut cut = live.len() - LIVE_MAX;
            while !live.is_char_boundary(cut) {
                cut += 1;
            }
            live.drain(..cut);
        }
        self.touch_tool(&t.call_id);
    }

    fn touch_tool(&mut self, call_id: &str) {
        if let Some(i) = self
            .entries
            .iter()
            .rposition(|e| matches!(e, Entry::Tool { call, .. } if call.id == call_id))
        {
            self.touch(i);
        }
    }

    pub fn tool_finished(&mut self, t: &ToolFinishedParams) {
        if t.duration_ms.is_some() {
            self.tool_meta
                .entry(t.call_id.clone())
                .or_default()
                .duration_ms = t.duration_ms;
        }
        self.set_tool_output(&t.call_id, t.output.clone(), t.is_error);
    }

    fn set_tool_output(&mut self, call_id: &str, out: String, is_error: bool) {
        let found = self
            .entries
            .iter()
            .rposition(|e| matches!(e, Entry::Tool { call, .. } if call.id == call_id));
        if let Some(i) = found {
            if let Entry::Tool { output, .. } = &mut self.entries[i] {
                *output = Some((out, is_error));
            }
            self.touch(i);
        }
    }

    pub fn turn_finished(&mut self, t: &TurnFinishedParams) {
        if self.turn.is_none_or(|r| r.turn_id == t.turn_id) {
            self.turn = None;
        }
        // A streaming entry left open (e.g. cancelled mid-stream) is final now.
        if let Some(i) = self.entries.iter().rposition(|e| {
            matches!(
                e,
                Entry::Assistant {
                    streaming: true,
                    ..
                }
            )
        }) {
            if let Entry::Assistant { streaming, .. } = &mut self.entries[i] {
                *streaming = false;
            }
            self.touch(i);
        }
        if let Some(user) = self
            .entries
            .iter()
            .rposition(|e| matches!(e, Entry::User(_)))
        {
            self.outcomes.insert(self.ids[user], t.outcome.clone());
        }
        match &t.outcome {
            TurnOutcome::Completed => {}
            TurnOutcome::Cancelled => self.push(Entry::Notice {
                text: "cancelled".into(),
                error: false,
            }),
            TurnOutcome::Failed { message } => self.push(Entry::Notice {
                text: format!("error: {message}"),
                error: true,
            }),
        }
    }

    /// A steered message joined the running turn.
    pub fn steered(&mut self, text: &str) {
        self.push(Entry::User(text.to_owned()));
    }

    /// The core's queue for this session changed.
    pub fn set_queue(&mut self, queue: Vec<QueuedMessage>, paused: bool) {
        if queue != self.queue || paused != self.queue_paused {
            self.queue = queue;
            self.queue_paused = paused;
            self.next_rev += 1;
            self.queue_rev = self.next_rev;
        }
    }

    pub fn notice(&mut self, text: String, error: bool) {
        self.push(Entry::Notice { text, error });
    }

    // ---- items ---------------------------------------------------------------

    /// The transcript as renderable items, in order.
    pub fn items(&self) -> Vec<Item> {
        let mut out = Vec::new();
        // Lua items in the order they go: by anchor, then as added.
        let mut lua: Vec<(usize, usize)> = self
            .lua_items
            .iter()
            .enumerate()
            .filter_map(|(slot, l)| l.as_ref().map(|l| (l.after.unwrap_or(0), slot)))
            .collect();
        lua.sort();
        let mut lua = lua.into_iter().peekable();
        // Those anchored before `next` (an id; None: all that are left).
        let mut add_lua = |next: Option<usize>, out: &mut Vec<Item>| {
            while let Some(&(after, slot)) = lua.peek() {
                if next.is_some_and(|n| after >= n) {
                    break;
                }
                out.push(Item {
                    entry: LUA_BASE + slot,
                    part: Part::Lua,
                });
                lua.next();
            }
        };
        add_lua(self.ids.first().copied(), &mut out);
        for (i, e) in self.entries.iter().enumerate() {
            let entry = self.ids[i];
            let mut add = |part| out.push(Item { entry, part });
            match e {
                Entry::User(_) => add(Part::User),
                Entry::Assistant {
                    text,
                    reasoning,
                    streaming,
                } => {
                    if !reasoning.trim().is_empty() {
                        add(Part::Reasoning);
                    }
                    // A streaming message with nothing yet still shows (as "…").
                    if !text.trim().is_empty() || (*streaming && reasoning.trim().is_empty()) {
                        add(Part::Assistant);
                    }
                }
                Entry::Tool { .. } => add(Part::Tool),
                Entry::Notice { .. } => add(Part::Notice),
            }
            add_lua(self.ids.get(i + 1).copied(), &mut out);
        }
        // Queued messages come last.
        out.extend((0..self.queue.len()).map(|i| Item {
            entry: QUEUE_BASE + i,
            part: Part::Queued,
        }));
        out
    }

    /// What `bone.chat.items` filters on besides kind and position, read
    /// without building the item's data: its tool name, whether it is
    /// running (a tool without output, text still streaming) and whether it
    /// failed (a failed tool, an error notice).
    pub fn item_status(&self, item: Item) -> (Option<std::borrow::Cow<'_, str>>, bool, bool) {
        let at = self.position(item.entry);
        match at.map(|at| &self.entries[at]) {
            _ if matches!(item.part, Part::Lua | Part::Queued) => {
                let d = self.item_fields(item, 0, false);
                let running = d["streaming"].as_bool().unwrap_or(false)
                    || (d["kind"] == "tool" && d["done"] == false);
                let error = d["is_error"].as_bool().unwrap_or(false)
                    || (d["kind"] == "notice" && d["error"] == true);
                let name = d["name"].as_str().map(|n| n.to_owned().into());
                (name, running, error)
            }
            Some(Entry::Tool { call, output }) => (
                Some(call.name.as_str().into()),
                output.is_none(),
                output.as_ref().is_some_and(|o| o.1),
            ),
            Some(Entry::Assistant { streaming, .. }) => (None, *streaming, false),
            Some(Entry::Notice { error, .. }) => (None, false, *error),
            Some(Entry::User(_)) | None => (None, false, false),
        }
    }

    /// An item as data for Lua. Text is cleaned of escape sequences, and
    /// message text loses blank lines at its edges (models often send them).
    /// Without `full`, a tool call leaves out its output, live output and
    /// raw arguments, the costly part of a long chat.
    pub fn item_data(&self, item: Item, index: usize, full: bool) -> Value {
        let mut d = self.item_fields(item, index, full);
        // A stable name for the item, for Lua to keep state by: it stays
        // the same as items come and go before it.
        d["key"] = json!(match item.part {
            Part::Lua => d["id"].as_str().unwrap_or_default().to_owned(),
            Part::Queued => format!("q{}", d["id"].as_str().unwrap_or_default()),
            _ => format!("e{}", item.entry),
        });
        d
    }

    fn item_fields(&self, item: Item, index: usize, full: bool) -> Value {
        if let Some(l) = self.lua_item(&item).filter(|_| item.part == Part::Lua) {
            let mut d = Value::Object(l.data.clone());
            d["kind"] = json!(l.kind);
            d["index"] = json!(index);
            d["id"] = json!(l.id);
            return d;
        }
        let kind = item.part.name();
        if item.part == Part::Queued {
            let position = item.entry - QUEUE_BASE;
            let q = &self.queue[position];
            let mode = match q.mode {
                QueueMode::Steer => "steer",
                QueueMode::Next => "next",
            };
            return json!({ "kind": kind, "index": index, "id": q.id, "text": edges(&q.text), "mode": mode, "position": position + 1 });
        }
        let Some(at) = self.position(item.entry) else {
            return json!({ "kind": kind, "index": index });
        };
        match (&self.entries[at], item.part) {
            (Entry::User(text), _) => json!({ "kind": kind, "index": index, "text": edges(text) }),
            (
                Entry::Assistant {
                    reasoning,
                    streaming,
                    ..
                },
                Part::Reasoning,
            ) => {
                json!({ "kind": kind, "index": index, "text": edges(reasoning), "streaming": streaming })
            }
            (
                Entry::Assistant {
                    text, streaming, ..
                },
                _,
            ) => {
                let mut d = json!({ "kind": kind, "index": index, "text": edges(text), "streaming": streaming });
                if let Some(u) = self.message_usage.get(&item.entry) {
                    d["usage"] = json!({ "input": u.input_tokens, "output": u.output_tokens });
                }
                d
            }
            (Entry::Tool { call, output }, _) => {
                let args: Value = serde_json::from_str(&call.arguments).unwrap_or(Value::Null);
                let meta = self.tool_meta.get(&call.id);
                let mut d = json!({
                    "kind": kind,
                    "index": index,
                    "id": call.id,
                    "name": call.name,
                    "arguments": args,
                    "is_error": output.as_ref().is_some_and(|o| o.1),
                    "done": output.is_some(),
                    "started_at": meta.and_then(|m| m.started_at),
                    "duration_ms": meta.and_then(|m| m.duration_ms),
                });
                if full {
                    d["raw_arguments"] = json!(clean(&call.arguments));
                    d["output"] = json!(output.as_ref().map(|o| clean(&o.0)));
                    d["live"] = json!(meta.map(|m| clean(&m.live)).filter(|l| !l.is_empty()));
                }
                if let Some(u) = self.message_usage.get(&item.entry) {
                    d["usage"] = json!({ "input": u.input_tokens, "output": u.output_tokens });
                }
                d
            }
            (Entry::Notice { text, error }, _) => {
                json!({ "kind": kind, "index": index, "text": edges(text), "error": error })
            }
        }
    }

    // ---- render cache --------------------------------------------------------

    /// Bumped on every change to this chat's data.
    pub fn rev(&self) -> u64 {
        self.next_rev
    }

    /// The rev of the data behind `item`.
    pub fn item_rev(&self, item: &Item) -> u64 {
        if item.part == Part::Lua {
            return self.lua_item(item).map_or(0, |l| l.rev);
        }
        match item.part {
            Part::Queued => self.queue_rev,
            _ => self.position(item.entry).map_or(0, |at| self.revs[at]),
        }
    }

    fn lua_item(&self, item: &Item) -> Option<&LuaItem> {
        self.lua_items
            .get(item.entry.checked_sub(LUA_BASE)?)?
            .as_ref()
    }

    /// The kind views know an item by: a Lua item's own kind, else its part.
    pub fn kind_name<'a>(&'a self, item: &Item) -> &'a str {
        match self.lua_item(item) {
            Some(l) if item.part == Part::Lua => &l.kind,
            _ => item.part.name(),
        }
    }

    /// Add a Lua item after the transcript as it is now.
    pub fn add_lua(&mut self, id: String, kind: String, data: serde_json::Map<String, Value>) {
        self.next_rev += 1;
        self.lua_items.push(Some(LuaItem {
            id,
            kind,
            data,
            after: self.ids.last().copied(),
            rev: self.next_rev,
        }));
    }

    /// Merge `fields` into a Lua item (a null field removes it). False if
    /// this chat has no item `id`.
    pub fn update_lua(&mut self, id: &str, fields: serde_json::Map<String, Value>) -> bool {
        if !self.lua_items.iter().flatten().any(|l| l.id == id) {
            return false;
        }
        self.next_rev += 1;
        let rev = self.next_rev;
        let Some(l) = self.lua_items.iter_mut().flatten().find(|l| l.id == id) else {
            return false;
        };
        for (k, v) in fields {
            if v.is_null() {
                l.data.remove(&k);
            } else {
                l.data.insert(k, v);
            }
        }
        l.rev = rev;
        true
    }

    pub fn remove_lua(&mut self, id: &str) -> bool {
        let Some(slot) = self
            .lua_items
            .iter()
            .position(|l| l.as_ref().is_some_and(|l| l.id == id))
        else {
            return false;
        };
        self.lua_items[slot] = None;
        // Reclaim trailing slots so add/remove churn (toasts, progress) doesn't grow it.
        while let Some(None) = self.lua_items.last() {
            self.lua_items.pop();
        }
        self.next_rev += 1;
        self.cache.remove(&Item {
            entry: LUA_BASE + slot,
            part: Part::Lua,
        });
        true
    }

    /// Cached items with dependencies not checked at data generation `data`.
    pub fn unchecked_deps(&self, data: u64) -> Vec<(Item, Vec<Dep>)> {
        self.cache
            .iter()
            .filter(|(_, c)| !c.deps.is_empty() && c.checked != data)
            .map(|(item, c)| (*item, c.deps.clone()))
            .collect()
    }

    pub fn mark_checked(&mut self, item: Item, data: u64) {
        if let Some(c) = self.cache.get_mut(&item) {
            c.checked = data;
        }
    }

    /// Draw `item` again on the next frame.
    pub fn invalidate(&mut self, item: Item) {
        self.cache.remove(&item);
    }

    /// Draw the item at 1-based `index` again, or every item.
    pub fn redraw(&mut self, index: Option<usize>) {
        match index {
            Some(i) => {
                if let Some(item) = i.checked_sub(1).and_then(|i| self.items().get(i).copied()) {
                    self.invalidate(item);
                }
            }
            None => self.cache.clear(),
        }
    }

    /// When the earliest cached item asked to be drawn again.
    pub fn next_expiry(&self) -> Option<Instant> {
        self.cache.values().filter_map(|c| c.expires).min()
    }

    /// Items whose cached lines are missing or out of date, with their key.
    pub fn stale(
        &self,
        width: usize,
        generation: (u64, u64),
        now: Instant,
    ) -> Vec<(Item, usize, RenderKey)> {
        let items = self.items();
        let rev = |item: &Item| self.item_rev(item);
        items
            .iter()
            .enumerate()
            .filter_map(|(i, item)| {
                let key = RenderKey {
                    width,
                    rev: rev(item),
                    generation,
                    prev: i.checked_sub(1).map(|p| items[p].part),
                };
                let fresh = self
                    .cache
                    .get(item)
                    .is_some_and(|c| c.key == key && c.expires.is_none_or(|e| e > now));
                (!fresh).then_some((*item, i + 1, key))
            })
            .collect()
    }

    pub fn store(
        &mut self,
        item: Item,
        key: RenderKey,
        lines: Vec<Line<'static>>,
        deps: Vec<Dep>,
        expires: Option<Instant>,
        checked: u64,
    ) {
        self.cache.insert(
            item,
            Cached {
                key,
                lines,
                deps,
                expires,
                checked,
            },
        );
    }

    /// All rendered rows, in order (after `stale` items have been stored).
    /// Lazy, and `skip` steps over whole items, so drawing one screen of a
    /// long chat doesn't walk every row.
    pub fn rows(&self) -> impl Iterator<Item = &Line<'static>> {
        (self.items().into_iter())
            .filter_map(|item| self.cache.get(&item))
            .flat_map(|c| c.lines.iter())
    }

    /// Where each drawn item sits: (1-based index, first row, row count).
    pub fn item_rows(&self) -> Vec<(usize, usize, usize)> {
        let mut row = 0;
        let mut out = Vec::new();
        for (n, item) in self.items().iter().enumerate() {
            if let Some(c) = self.cache.get(item) {
                out.push((n + 1, row, c.lines.len()));
                row += c.lines.len();
            }
        }
        out
    }

    /// The item drawn on chat row `row` (0-based, from the top of the
    /// transcript) and the 1-based line within it.
    pub fn item_at_row(&self, row: usize) -> Option<(usize, usize)> {
        self.item_rows()
            .into_iter()
            .find(|&(_, start, len)| row >= start && row < start + len)
            .map(|(index, start, _)| (index, row - start + 1))
    }

    pub fn row_count(&self) -> usize {
        self.items()
            .iter()
            .filter_map(|i| self.cache.get(i))
            .map(|c| c.lines.len())
            .sum()
    }
}

/// `clean`, without blank lines at the start or whitespace at the end.
fn edges(s: &str) -> String {
    let s = clean(s);
    let start = s
        .char_indices()
        .take_while(|(_, c)| c.is_whitespace())
        .filter(|(_, c)| *c == '\n')
        .last()
        .map_or(0, |(i, _)| i + 1);
    s[start..].trim_end().to_owned()
}

/// The bare rendering used when no Lua view applies: plain wrapped text with
/// a blank line between items, no colors. Reasoning is hidden (a Lua view
/// can show it). `first` is whether this is the first item.
pub fn bare(data: &Value, width: usize, first: bool) -> Vec<Line<'static>> {
    let s = |k: &str| data[k].as_str().unwrap_or("").to_owned();
    let text = match data["kind"].as_str().unwrap_or("") {
        "reasoning" => return Vec::new(),
        "user" => format!("> {}", s("text")),
        "tool" => {
            let mut t = format!("{} {}", s("name"), s("raw_arguments"));
            if let Some(out) = data["output"].as_str() {
                t.push('\n');
                t.push_str(out);
            }
            t
        }
        "notice" => format!("! {}", s("text")),
        "queued" => format!("(queued) {}", s("text")),
        _ => s("text"),
    };
    if text.is_empty() {
        return Vec::new();
    }
    let gap = (!first).then(Line::default);
    gap.into_iter()
        .chain(
            sanitize(&text)
                .split('\n')
                .flat_map(|l| wrap(l, width))
                .map(Line::from),
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(id: &str, args: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: "shell".into(),
            arguments: args.into(),
        }
    }

    /// Render with the bare fallback, like a UI without Lua.
    fn rows(c: &mut ChatBuffer, width: usize) -> Vec<String> {
        for (item, index, key) in c.stale(width, (0, 0), Instant::now()) {
            let lines = bare(&c.item_data(item, index, true), width, index == 1);
            c.store(item, key, lines, Vec::new(), None, 0);
        }
        c.rows().map(|l| l.to_string()).collect()
    }

    #[test]
    fn items_and_data() {
        let mut c = ChatBuffer::new(None);
        c.turn_started(1, "hi");
        let parts = |c: &ChatBuffer| c.items().iter().map(|i| i.part.name()).collect::<Vec<_>>();
        // Streaming with nothing yet: an empty assistant item.
        c.delta(&MessageDeltaParams {
            session_id: "s".into(),
            turn_id: 1,
            kind: DeltaKind::Text,
            text: String::new(),
        });
        assert_eq!(parts(&c), ["user", "assistant"]);
        c.delta(&MessageDeltaParams {
            session_id: "s".into(),
            turn_id: 1,
            kind: DeltaKind::Reasoning,
            text: "think".into(),
        });
        assert_eq!(parts(&c), ["user", "reasoning"]);
        c.message_completed(&MessageCompletedParams {
            session_id: "s".into(),
            turn_id: 1,
            message: ChatMessage::Assistant {
                content: "**Hello!**".into(),
                reasoning: "think".into(),
                tool_calls: vec![call("c1", r#"{"command":"ls"}"#)],
            },
            usage: Some(Usage {
                input_tokens: 5,
                output_tokens: 2,
                cached_tokens: None,
            }),
        });
        assert_eq!(parts(&c), ["user", "reasoning", "assistant", "tool"]);
        let items = c.items();
        let tool = c.item_data(items[3], 4, true);
        assert_eq!(
            (
                tool["name"].as_str(),
                tool["arguments"]["command"].as_str(),
                tool["done"].as_bool()
            ),
            (Some("shell"), Some("ls"), Some(false))
        );
        c.tool_finished(&ToolFinishedParams {
            session_id: "s".into(),
            turn_id: 1,
            call_id: "c1".into(),
            output: "a\x1b[31mb\x1b[0m\tc".into(),
            is_error: false,
            duration_ms: None,
        });
        assert_eq!(c.item_data(items[3], 4, true)["output"], "ab\tc");
    }

    #[test]
    fn entries_keep_their_ids_while_a_message_streams() {
        let mut c = ChatBuffer::new(None);
        c.turn_started(1, "go");
        c.add_lua("note-1".into(), "note".into(), Default::default());
        c.delta(&MessageDeltaParams {
            session_id: "s".into(),
            turn_id: 1,
            kind: DeltaKind::Text,
            text: "hi".into(),
        });
        let keys = |c: &ChatBuffer| -> Vec<String> {
            c.items()
                .iter()
                .enumerate()
                .map(|(n, i)| {
                    c.item_data(*i, n + 1, false)["key"]
                        .as_str()
                        .unwrap()
                        .to_owned()
                })
                .collect()
        };
        assert_eq!(keys(&c), ["e1", "note-1", "e2"]);
        // The streamed entry is replaced by the final message: a new id, the
        // others unchanged and the Lua item still after the user message.
        c.message_completed(&MessageCompletedParams {
            session_id: "s".into(),
            turn_id: 1,
            message: ChatMessage::Assistant {
                content: "hi".into(),
                reasoning: String::new(),
                tool_calls: vec![],
            },
            usage: Some(Usage {
                input_tokens: 5,
                output_tokens: 1,
                cached_tokens: None,
            }),
        });
        assert_eq!(keys(&c), ["e1", "note-1", "e3"]);
        assert_eq!(c.item_data(c.items()[2], 3, false)["usage"]["input"], 5);
        c.turn_finished(&TurnFinishedParams {
            session_id: "s".into(),
            turn_id: 1,
            outcome: TurnOutcome::Completed,
        });
        assert!(c.outcomes.contains_key(&1));
    }

    #[test]
    fn cache_redoes_only_what_changed() {
        let mut c = ChatBuffer::new(None);
        c.turn_started(1, "one two three");
        c.notice("x".into(), false);
        assert_eq!(rows(&mut c, 40), ["> one two three", "", "! x"]);
        assert!(c.stale(40, (0, 0), Instant::now()).is_empty());
        // Width and generation changes redo everything.
        assert_eq!(c.stale(9, (0, 0), Instant::now()).len(), 2);
        assert_eq!(c.stale(40, (1, 0), Instant::now()).len(), 2);
        assert_eq!(rows(&mut c, 9), ["> one two", "three", "", "! x"]);
        // A change to one entry redoes only its item.
        c.delta(&MessageDeltaParams {
            session_id: "s".into(),
            turn_id: 1,
            kind: DeltaKind::Text,
            text: "hey".into(),
        });
        assert_eq!(c.stale(9, (0, 0), Instant::now()).len(), 1);
        assert_eq!(c.row_count(), 4);
    }

    #[test]
    fn load_pairs_tool_results() {
        let mut c = ChatBuffer::new(None);
        let info = SessionInfo {
            session_id: "s".into(),
            cwd: "/".into(),
            created_at: 0,
            title: Some("t".into()),
            parent: None,
            owner: None,
        };
        c.load(
            info,
            &[
                ChatMessage::User {
                    content: "go".into(),
                },
                ChatMessage::Assistant {
                    content: String::new(),
                    reasoning: String::new(),
                    tool_calls: vec![call("c1", r#"{"command":"x"}"#)],
                },
                ChatMessage::Tool {
                    call_id: "c1".into(),
                    content: "boom".into(),
                    is_error: true,
                },
            ],
            Some(3),
        );
        assert_eq!(
            rows(&mut c, 40),
            ["> go", "", r#"shell {"command":"x"}"#, "boom"]
        );
        assert_eq!(c.turn.unwrap().turn_id, 3);
        assert_eq!(c.title(), "t");
    }
}

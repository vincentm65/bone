//! Read-only chat data for Lua (`bone.chat`): items, turns and sessions of
//! the chats this TUI has open. Everything is a fresh copy built from the
//! transcript; changing it changes nothing. The core stays the authority:
//! `bone.chat.messages` asks it for the stored transcript.

use bone_proto::types::TurnOutcome;
use serde_json::{Value as Json, json};

use crate::app::App;
use crate::chat::{ChatBuffer, Item, Part};

/// Which items `bone.chat.items` returns. Indexes are 1-based, like `index`
/// on every item.
#[derive(Debug, Default, Clone)]
pub struct ItemFilter {
    /// One kind, or any of several.
    pub kind: Option<Vec<String>>,
    /// Only the unbroken stretch of items (of `kind`) around this index.
    pub around: Option<usize>,
    /// Tool name.
    pub name: Option<String>,
    pub turn: Option<usize>,
    /// Tools still running, text still streaming.
    pub running: Option<bool>,
    /// Failed tools and error notices.
    pub error: Option<bool>,
    /// Item index range, inclusive.
    pub from: Option<usize>,
    pub to: Option<usize>,
    /// Keep the first or last N matches.
    pub first: Option<usize>,
    pub last: Option<usize>,
    /// Another open chat, by session id (default: the one on screen).
    pub session: Option<String>,
    /// Include tool output, live output and raw arguments.
    pub full: bool,
}

/// A `bone.chat.items` query a chat view made while drawing, and a digest of
/// what it could see. The view is drawn again when the digest changes.
#[derive(Debug, Clone)]
pub struct Dep {
    pub filter: ItemFilter,
    pub sig: u64,
}

/// A chat's items and their turns, built once for many queries.
pub struct Indexed<'a> {
    chat: &'a ChatBuffer,
    items: Vec<Item>,
    turns: Vec<usize>,
}

impl Indexed<'_> {
    /// The items (0-based) that pass the filter's kind, turn, range and
    /// `around` tests: those that need no item data.
    fn candidates<'f>(&'f self, f: &'f ItemFilter) -> impl Iterator<Item = usize> + 'f {
        let (chat, items) = (self.chat, &self.items);
        let kind_ok = move |i: &Item| {
            f.kind
                .as_ref()
                .is_none_or(|ks| ks.iter().any(|k| chat.kind_name(i) == k))
        };
        // `around`: grow from the item while neighbours are of `kind`.
        let (lo, hi) = match f.around.and_then(|a| a.checked_sub(1)) {
            Some(at) if at < items.len() && kind_ok(&items[at]) => {
                let mut lo = at;
                while lo > 0 && kind_ok(&items[lo - 1]) {
                    lo -= 1;
                }
                let mut hi = at + 1;
                while hi < items.len() && kind_ok(&items[hi]) {
                    hi += 1;
                }
                (lo, hi)
            }
            Some(_) => (0, 0),
            None => (0, items.len()),
        };
        // `from`/`to` narrow the walk itself: a one-item query is one step.
        let lo = lo.max(f.from.map_or(0, |from| from.saturating_sub(1)));
        let hi = hi.min(f.to.unwrap_or(usize::MAX));
        (lo..hi.max(lo))
            .filter(move |&n| kind_ok(&items[n]) && f.turn.is_none_or(|t| self.turns[n] == t))
    }

    /// A digest of the items `f` could match: those its kind, turn, range
    /// and `around` allow, with their revs. Coarser than the result (name,
    /// running, error, first and last are left out), so it may redraw too
    /// often, never too rarely.
    pub fn signature(&self, f: &ItemFilter) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        let c = self.chat;
        for n in self.candidates(f) {
            let i = &self.items[n];
            (n + 1, i.entry, c.kind_name(i), c.item_rev(i)).hash(&mut h);
        }
        h.finish()
    }
}

/// Turn number of each item: 0 before the first user message, then 1, 2, …
fn turns_of(items: &[Item]) -> Vec<usize> {
    let mut n = 0;
    items
        .iter()
        .map(|i| {
            n += usize::from(i.part == Part::User);
            n
        })
        .collect()
}

fn outcome(o: Option<&TurnOutcome>) -> (Json, Json) {
    match o {
        None => (Json::Null, Json::Null),
        Some(TurnOutcome::Completed) => (json!("completed"), Json::Null),
        Some(TurnOutcome::Cancelled) => (json!("cancelled"), Json::Null),
        Some(TurnOutcome::Failed { message }) => (json!("failed"), json!(message)),
    }
}

impl App {
    /// The chat a query is about: by session id, else the one on screen.
    /// `None` for a session no open chat has.
    pub fn data_chat(&self, session: Option<&str>) -> Option<&ChatBuffer> {
        match session {
            Some(id) => self.chat_by_session(id).and_then(|b| self.chat(b)),
            None => self.chat(self.current),
        }
    }

    /// Items as data, with their `turn`, filtered. Only the items returned
    /// are built as data.
    pub fn chat_query(&self, f: &ItemFilter) -> Vec<Json> {
        let Some(c) = self.indexed(f.session.as_deref()) else {
            return Vec::new();
        };
        let picked: Vec<usize> = (c.candidates(f))
            .filter(|&n| {
                if f.name.is_none() && f.running.is_none() && f.error.is_none() {
                    return true;
                }
                let (name, running, error) = c.chat.item_status(c.items[n]);
                f.name.as_deref().is_none_or(|n| name.as_deref() == Some(n))
                    && f.running.is_none_or(|r| r == running)
                    && f.error.is_none_or(|e| e == error)
            })
            .collect();
        let skip = picked.len().saturating_sub(f.last.unwrap_or(usize::MAX));
        (picked.into_iter().skip(skip))
            .take(f.first.unwrap_or(usize::MAX))
            .map(|n| {
                let mut d = c.chat.item_data(c.items[n], n + 1, f.full);
                d["turn"] = json!(c.turns[n]);
                d
            })
            .collect()
    }

    /// The chat a query is about, indexed for [`Indexed::signature`].
    pub fn indexed(&self, session: Option<&str>) -> Option<Indexed<'_>> {
        let chat = self.data_chat(session)?;
        let items = chat.items();
        let turns = turns_of(&items);
        Some(Indexed { chat, items, turns })
    }

    /// See [`Indexed::signature`]; 0 for a chat that isn't open.
    pub fn chat_signature(&self, f: &ItemFilter) -> u64 {
        self.indexed(f.session.as_deref())
            .map_or(0, |c| c.signature(f))
    }

    /// Changes whenever any open chat's data does.
    pub fn data_generation(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for c in &self.chats {
            c.rev().hash(&mut h);
        }
        h.finish()
    }

    /// The turns of a chat: each user message and what followed it.
    pub fn chat_turns(&self, session: Option<&str>) -> Vec<Json> {
        let Some(chat) = self.data_chat(session) else {
            return Vec::new();
        };
        let items = chat.items();
        let turns = turns_of(&items);
        let count = turns.last().copied().unwrap_or(0);
        let running = chat.turn.is_some() || chat.starting;
        (1..=count)
            .map(|t| {
                let members: Vec<(usize, &Item)> = items
                    .iter()
                    .enumerate()
                    .filter(|(n, _)| turns[*n] == t)
                    .collect();
                let (first, user) = members[0];
                let data: Vec<Json> = members
                    .iter()
                    .map(|(n, i)| chat.item_data(**i, n + 1, false))
                    .collect();
                let tools = data.iter().filter(|d| d["kind"] == "tool");
                let (outcome, error) = outcome(chat.outcomes.get(&user.entry));
                json!({
                    "index": t,
                    "text": data[0]["text"],
                    "first": first + 1,
                    "last": members.last().map_or(first, |m| m.0) + 1,
                    "items": members.len(),
                    "tools": tools.clone().count(),
                    "tool_errors": tools.filter(|d| d["is_error"] == true).count(),
                    "running": t == count && running,
                    "outcome": outcome,
                    "error": error,
                })
            })
            .collect()
    }

    /// One chat's session: the core's info plus what the TUI knows.
    pub fn chat_session(&self, session: Option<&str>) -> Json {
        let Some(chat) = self.data_chat(session) else {
            return Json::Null;
        };
        let current = std::ptr::eq(chat, &self.chats[self.current]);
        let items = chat.items();
        let mut v = match &chat.session {
            Some(info) => serde_json::to_value(info).unwrap_or_else(|_| json!({})),
            None => json!({}),
        };
        v["title"] = json!(chat.title());
        v["new"] = json!(chat.session.is_none());
        v["current"] = json!(current);
        v["running"] = json!(chat.turn.is_some() || chat.starting);
        v["starting"] = json!(chat.starting);
        v["turn"] = chat.turn.map_or(
            Json::Null,
            |t| json!({ "id": t.turn_id, "elapsed_ms": t.started.elapsed().as_millis() as u64 }),
        );
        v["usage"] = chat.usage.map_or(
            Json::Null,
            |u| json!({ "input": u.input_tokens, "output": u.output_tokens }),
        );
        v["items"] = json!(items.len());
        v["turns"] = json!(turns_of(&items).last().copied().unwrap_or(0));
        v
    }

    /// Every chat this TUI has open, the one on screen marked `current`.
    pub fn chat_sessions(&self) -> Vec<Json> {
        self.chats
            .iter()
            .enumerate()
            .map(|(b, c)| {
                json!({
                    "session_id": c.session_id(),
                    "title": c.title(),
                    "new": c.session.is_none(),
                    "current": b == self.current,
                    "running": c.turn.is_some() || c.starting,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_number_their_turns() {
        let item = |part| Item { entry: 0, part };
        let items = [
            item(Part::Notice),
            item(Part::User),
            item(Part::Tool),
            item(Part::User),
            item(Part::Assistant),
        ];
        assert_eq!(turns_of(&items), [0, 1, 1, 2, 2]);
    }
}

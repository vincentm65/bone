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
}

/// A `bone.chat.items` query a chat view made while drawing, and a digest of
/// what it could see. The view is drawn again when the digest changes.
#[derive(Debug, Clone)]
pub struct Dep {
    pub filter: ItemFilter,
    pub sig: u64,
}

/// Whether item `n` (0-based) of `items` passes the filter's kind, turn,
/// range and `around` tests: those that need no item data.
fn candidates(chat: &ChatBuffer, items: &[Item], turns: &[usize], f: &ItemFilter) -> Vec<bool> {
    let kind_ok = |i: &Item| {
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
            let mut hi = at;
            while hi + 1 < items.len() && kind_ok(&items[hi + 1]) {
                hi += 1;
            }
            (lo, hi)
        }
        Some(_) => (1, 0),
        None => (0, usize::MAX),
    };
    items
        .iter()
        .enumerate()
        .map(|(n, i)| {
            let index = n + 1;
            n >= lo
                && n <= hi
                && kind_ok(i)
                && f.turn.is_none_or(|t| turns[n] == t)
                && f.from.is_none_or(|from| index >= from)
                && f.to.is_none_or(|to| index <= to)
        })
        .collect()
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

    /// Items as data, with their `turn`, filtered.
    pub fn chat_query(&self, f: &ItemFilter) -> Vec<Json> {
        let Some(chat) = self.data_chat(f.session.as_deref()) else {
            return Vec::new();
        };
        let items = chat.items();
        let turns = turns_of(&items);
        let ok = candidates(chat, &items, &turns, f);
        let picked: Vec<Json> = items
            .iter()
            .enumerate()
            .filter(|(n, _)| ok[*n])
            .map(|(n, i)| {
                let mut d = chat.item_data(*i, n + 1);
                d["turn"] = json!(turns[n]);
                d
            })
            .filter(|d| {
                let running = d["streaming"].as_bool().unwrap_or(false)
                    || (d["kind"] == "tool" && d["done"] == false);
                let error = d["is_error"].as_bool().unwrap_or(false)
                    || (d["kind"] == "notice" && d["error"] == true);
                f.name.as_deref().is_none_or(|n| d["name"] == n)
                    && f.running.is_none_or(|r| r == running)
                    && f.error.is_none_or(|e| e == error)
            })
            .collect();
        let skip = picked.len().saturating_sub(f.last.unwrap_or(usize::MAX));
        picked
            .into_iter()
            .skip(skip)
            .take(f.first.unwrap_or(usize::MAX))
            .collect()
    }

    /// A digest of the items `f` could match: those its kind, turn, range
    /// and `around` allow, with their revs. Coarser than the result (name, running, error,
    /// first and last are left out), so it may redraw too often, never too
    /// rarely.
    pub fn chat_signature(&self, f: &ItemFilter) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        let Some(chat) = self.data_chat(f.session.as_deref()) else {
            return 0;
        };
        let items = chat.items();
        let turns = turns_of(&items);
        let ok = candidates(chat, &items, &turns, f);
        for (n, i) in items.iter().enumerate() {
            if ok[n] {
                (n + 1, i.entry, chat.kind_name(i), chat.item_rev(i)).hash(&mut h);
            }
        }
        h.finish()
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
                    .map(|(n, i)| chat.item_data(**i, n + 1))
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

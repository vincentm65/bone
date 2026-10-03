//! Chat buffers: a session transcript built from protocol events.
//!
//! This module only holds data and a render cache. A transcript is a list of
//! *items* (user, reasoning, assistant, tool, notice); how each one looks is
//! decided elsewhere (Lua views, see `ui.rs`). Rendered lines are cached per
//! item and redone only when the item, the width or the views change.

use std::collections::HashMap;
use std::time::Instant;

use bone_proto::methods::{
    MessageCompletedParams, MessageDeltaParams, ToolFinishedParams, ToolStartedParams,
    TurnFinishedParams,
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
}

impl Part {
    pub fn name(self) -> &'static str {
        match self {
            Part::User => "user",
            Part::Reasoning => "reasoning",
            Part::Assistant => "assistant",
            Part::Tool => "tool",
            Part::Notice => "notice",
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
    pub usage: Option<Usage>,
    /// A prompt was sent and the turn has not started yet.
    pub starting: bool,
    /// How finished turns ended, by the entry index of their user message.
    /// Only turns seen finishing here; loaded history has none.
    pub outcomes: HashMap<usize, TurnOutcome>,
    /// Bumped on every change to an entry; part of each cache key.
    revs: Vec<u64>,
    next_rev: u64,
    cache: HashMap<Item, (RenderKey, Vec<Line<'static>>)>,
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
        self.next_rev += 1;
        self.revs.push(self.next_rev);
    }

    fn pop(&mut self) {
        let i = self.entries.len() - 1;
        self.entries.pop();
        self.revs.pop();
        self.cache.retain(|item, _| item.entry != i);
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
        self.revs.clear();
        self.cache.clear();
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
        self.add_assistant_message(&m.message);
        if m.usage.is_some() {
            self.usage = m.usage;
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

    pub fn tool_finished(&mut self, t: &ToolFinishedParams) {
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
            self.outcomes.insert(user, t.outcome.clone());
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

    pub fn notice(&mut self, text: String, error: bool) {
        self.push(Entry::Notice { text, error });
    }

    // ---- items ---------------------------------------------------------------

    /// The transcript as renderable items, in order.
    pub fn items(&self) -> Vec<Item> {
        let mut out = Vec::new();
        for (entry, e) in self.entries.iter().enumerate() {
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
        }
        out
    }

    /// An item as data for Lua. Text is cleaned of escape sequences, and
    /// message text loses blank lines at its edges (models often send them).
    pub fn item_data(&self, item: Item, index: usize) -> Value {
        let kind = item.part.name();
        match (&self.entries[item.entry], item.part) {
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
                json!({ "kind": kind, "index": index, "text": edges(text), "streaming": streaming })
            }
            (Entry::Tool { call, output }, _) => {
                let args: Value = serde_json::from_str(&call.arguments).unwrap_or(Value::Null);
                json!({
                    "kind": kind,
                    "index": index,
                    "id": call.id,
                    "name": call.name,
                    "arguments": args,
                    "raw_arguments": clean(&call.arguments),
                    "output": output.as_ref().map(|o| clean(&o.0)),
                    "is_error": output.as_ref().is_some_and(|o| o.1),
                    "done": output.is_some(),
                })
            }
            (Entry::Notice { text, error }, _) => {
                json!({ "kind": kind, "index": index, "text": edges(text), "error": error })
            }
        }
    }

    // ---- render cache --------------------------------------------------------

    /// Items whose cached lines are missing or out of date, with their key.
    pub fn stale(&self, width: usize, generation: (u64, u64)) -> Vec<(Item, usize, RenderKey)> {
        let items = self.items();
        items
            .iter()
            .enumerate()
            .filter_map(|(i, item)| {
                let key = RenderKey {
                    width,
                    rev: self.revs[item.entry],
                    generation,
                    prev: i.checked_sub(1).map(|p| items[p].part),
                };
                let fresh = self.cache.get(item).is_some_and(|(k, _)| *k == key);
                (!fresh).then_some((*item, i + 1, key))
            })
            .collect()
    }

    pub fn store(&mut self, item: Item, key: RenderKey, lines: Vec<Line<'static>>) {
        self.cache.insert(item, (key, lines));
    }

    /// All rendered rows, in order (after `stale` items have been stored).
    pub fn rows(&self) -> impl Iterator<Item = &Line<'static>> {
        self.items()
            .into_iter()
            .filter_map(|item| self.cache.get(&item))
            .flat_map(|(_, lines)| lines.iter())
            .collect::<Vec<_>>()
            .into_iter()
    }

    pub fn row_count(&self) -> usize {
        self.items()
            .iter()
            .filter_map(|i| self.cache.get(i))
            .map(|(_, l)| l.len())
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
        for (item, index, key) in c.stale(width, (0, 0)) {
            let lines = bare(&c.item_data(item, index), width, index == 1);
            c.store(item, key, lines);
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
            }),
        });
        assert_eq!(parts(&c), ["user", "reasoning", "assistant", "tool"]);
        let items = c.items();
        let tool = c.item_data(items[3], 4);
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
        });
        assert_eq!(c.item_data(items[3], 4)["output"], "ab\tc");
        assert_eq!(c.usage.unwrap().input_tokens, 5);
    }

    #[test]
    fn cache_redoes_only_what_changed() {
        let mut c = ChatBuffer::new(None);
        c.turn_started(1, "one two three");
        c.notice("x".into(), false);
        assert_eq!(rows(&mut c, 40), ["> one two three", "", "! x"]);
        assert!(c.stale(40, (0, 0)).is_empty());
        // Width and generation changes redo everything.
        assert_eq!(c.stale(9, (0, 0)).len(), 2);
        assert_eq!(c.stale(40, (1, 0)).len(), 2);
        assert_eq!(rows(&mut c, 9), ["> one two", "three", "", "! x"]);
        // A change to one entry redoes only its item.
        c.delta(&MessageDeltaParams {
            session_id: "s".into(),
            turn_id: 1,
            kind: DeltaKind::Text,
            text: "hey".into(),
        });
        assert_eq!(c.stale(9, (0, 0)).len(), 1);
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

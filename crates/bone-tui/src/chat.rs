//! Chat buffers (from `bone-ui`) caching ratatui lines, and the bare
//! rendering used without Lua views.

use ratatui::text::Line;
use serde_json::Value;

pub use bone_ui::chat::{Item, RenderKey, RunningTurn};

use crate::text::{sanitize, wrap};

pub type ChatBuffer = bone_ui::chat::ChatBuffer<Line<'static>>;

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
    use std::time::Instant;

    use bone_proto::methods::MessageDeltaParams;
    use bone_proto::types::{ChatMessage, DeltaKind, SessionInfo, ToolCall};

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
                    images: Vec::new(),
                },
                ChatMessage::Assistant {
                    content: String::new(),
                    reasoning: String::new(),
                    tool_calls: vec![call("c1", r#"{"command":"x"}"#)],
                    reasoning_state: None,
                },
                ChatMessage::Tool {
                    images: Vec::new(),
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

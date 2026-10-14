//! Keep transcripts valid for model providers.

use bone_proto::types::ChatMessage;

const NO_RESULT: &str = "[no saved result: tool execution was interrupted; outcome unknown. Check current state before retrying side effects.]";

/// Keeps a transcript as providers want it: each tool call answered before
/// anything else comes, and no result without its call.
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
                images: Vec::new(),
                call_id,
                content: NO_RESULT.into(),
                is_error: true,
            })
            .collect()
    }
}

pub(crate) fn repair(messages: Vec<ChatMessage>) -> Vec<ChatMessage> {
    let mut closer = Closer::default();
    let mut out: Vec<ChatMessage> = messages.into_iter().flat_map(|m| closer.next(m)).collect();
    out.extend(closer.close());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use bone_proto::types::ToolCall;

    #[test]
    fn valid_transcripts_are_unchanged() {
        assert!(repair(Vec::new()).is_empty());
        let messages = vec![
            ChatMessage::User {
                content: "hi".into(),
                images: Vec::new(),
            },
            ChatMessage::Assistant {
                content: String::new(),
                reasoning: "check first".into(),
                tool_calls: vec![ToolCall {
                    id: "a".into(),
                    name: "shell".into(),
                    arguments: "{}".into(),
                }],
                reasoning_state: None,
            },
            ChatMessage::Tool {
                images: Vec::new(),
                call_id: "a".into(),
                content: "ok".into(),
                is_error: false,
            },
        ];
        assert_eq!(repair(messages.clone()), messages);
    }

    #[test]
    fn transcripts_are_repaired_for_providers() {
        let call = |id: &str| ToolCall {
            id: id.into(),
            name: "shell".into(),
            arguments: "{}".into(),
        };
        let result = |id: &str| ChatMessage::Tool {
            images: Vec::new(),
            call_id: id.into(),
            content: "ok".into(),
            is_error: false,
        };
        let user = ChatMessage::User {
            content: "hi".into(),
            images: Vec::new(),
        };
        let asked = ChatMessage::Assistant {
            content: String::new(),
            reasoning: String::new(),
            tool_calls: vec![call("a"), call("b")],
            reasoning_state: None,
        };
        let fixed = repair(vec![
            result("x"),
            asked.clone(),
            result("b"),
            user.clone(),
            asked.clone(),
        ]);
        let missing = |id: &str| ChatMessage::Tool {
            images: Vec::new(),
            call_id: id.into(),
            content: NO_RESULT.into(),
            is_error: true,
        };
        assert_eq!(
            fixed,
            [
                asked.clone(),
                result("b"),
                missing("a"),
                user,
                asked,
                missing("a"),
                missing("b"),
            ]
        );
    }
}

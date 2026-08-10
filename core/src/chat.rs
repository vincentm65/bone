//! Conversation model: on-disk message representation and provider chat-history assembly.

use crate::llm::{ChatMessage, ChatRole};

// ── History ─────────────────────────────────────────────────────────────────

/// Build provider history without truncating conversation or tool chains.
pub fn build_chat_history(messages: &[ChatMessage], system_prompt: &str) -> Vec<ChatMessage> {
    let mut out = Vec::with_capacity(messages.len() + 1);
    out.push(ChatMessage::new(ChatRole::System, system_prompt));
    let mut requested_at = None;
    for message in messages {
        out.push(model_facing_message(message, requested_at.as_deref()));
        if message.role == ChatRole::Assistant && !message.tool_calls.is_empty() {
            requested_at = message.created_at.clone();
        }
    }
    out
}

/// Clone a transcript message and add timing context only to the provider copy.
/// Assistant messages are left unchanged so timing metadata does not become an
/// assistant-output pattern that models repeat in subsequent responses.
pub(crate) fn model_facing_message(
    message: &ChatMessage,
    requested_at: Option<&str>,
) -> ChatMessage {
    let mut message = message.clone();
    let timing = match message.role {
        ChatRole::Assistant => return message,
        ChatRole::Tool => match (requested_at, message.created_at.as_deref()) {
            (Some(requested_at), Some(completed_at)) => {
                format!("Tool timing: requested at {requested_at}; completed at {completed_at}.")
            }
            (Some(requested_at), None) => format!("Tool timing: requested at {requested_at}."),
            (None, Some(completed_at)) => format!("Tool timing: completed at {completed_at}."),
            (None, None) => return message,
        },
        _ => {
            let Some(created_at) = message.created_at.as_deref() else {
                return message;
            };
            format!("Message timestamp: {created_at}.")
        }
    };
    if !message.content.is_empty() {
        message.content.push_str("\n\n");
    }
    message
        .content
        .push_str(&format!("<timing>{timing}</timing>"));
    message
}

// ── Message ─────────────────────────────────────────────────────────────────

/// Display metadata for compact tool rows shown in chat.
#[derive(Debug, Clone)]
pub struct ToolDisplay {
    pub label: String,
    pub is_error: bool,
    pub is_shell: bool,
}

/// A single chat message.
#[derive(Debug, Clone)]
pub struct Message {
    pub role: ChatRole,
    pub content: String,
    /// Present when this message represents a tool call or result.
    pub tool: Option<ToolDisplay>,
    pub image_count: usize,
}

impl Message {
    #[must_use]
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::User,
            content: content.into(),
            tool: None,
            image_count: 0,
        }
    }

    #[must_use]
    pub fn user_with_images(content: impl Into<String>, image_count: usize) -> Self {
        Self {
            role: ChatRole::User,
            content: content.into(),
            tool: None,
            image_count,
        }
    }

    #[must_use]
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::Assistant,
            content: content.into(),
            tool: None,
            image_count: 0,
        }
    }

    #[must_use]
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::System,
            content: content.into(),
            tool: None,
            image_count: 0,
        }
    }

    #[must_use]
    pub fn tool_row(label: String, is_error: bool) -> Self {
        Self {
            role: ChatRole::Tool,
            content: String::new(),
            tool: Some(ToolDisplay {
                label,
                is_error,
                is_shell: false,
            }),
            image_count: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolCall;

    #[test]
    fn provider_history_adds_tool_timing_without_mutating_transcript() {
        let mut assistant = ChatMessage::new(ChatRole::Assistant, "checking");
        assistant.created_at = Some("2026-07-17T12:00:00Z".into());
        assistant.tool_calls.push(ToolCall {
            id: "call-1".into(),
            name: "shell".into(),
            arguments: serde_json::json!({"command": "sleep 5"}),
        });
        let mut tool = ChatMessage::new(ChatRole::Tool, "done");
        tool.tool_call_id = Some("call-1".into());
        tool.name = Some("shell".into());
        tool.created_at = Some("2026-07-17T12:00:05Z".into());
        let transcript = vec![assistant, tool];

        let history = build_chat_history(&transcript, "system");

        assert_eq!(transcript[0].content, "checking");
        assert_eq!(transcript[1].content, "done");
        assert_eq!(history[1].content, "checking");
        assert_eq!(history[1].output_sequence, transcript[0].output_sequence);
        assert!(history[2].content.contains(
            "Tool timing: requested at 2026-07-17T12:00:00Z; completed at 2026-07-17T12:00:05Z."
        ));
    }

    #[test]
    fn provider_history_does_not_add_timing_to_assistant_output() {
        let mut assistant = ChatMessage::new(ChatRole::Assistant, "");
        assistant.created_at = Some("2026-07-17T12:00:00Z".into());
        assistant.output_sequence = vec![crate::llm::OutputItem::ToolCall(ToolCall {
            id: "call-1".into(),
            name: "shell".into(),
            arguments: serde_json::json!({"command": "true"}),
        })];

        let history = build_chat_history(&[assistant.clone()], "system");

        assert_eq!(history[1].content, "");
        assert_eq!(history[1].output_sequence, assistant.output_sequence);
        let codex_items = crate::llm::providers::codex::build_codex_messages(history);
        assert!(
            !serde_json::to_string(&codex_items)
                .unwrap()
                .contains("<timing>")
        );
    }
}

use crate::llm::prompts;
use crate::llm::{ChatMessage, ChatRole};

// ── History ─────────────────────────────────────────────────────────────────

/// Build provider history without truncating conversation or tool chains.
pub fn build_chat_history(messages: &[ChatMessage]) -> Vec<ChatMessage> {
    let mut out = Vec::with_capacity(messages.len() + 1);
    out.push(ChatMessage::new(ChatRole::System, prompts::system_prompt()));
    out.extend(messages.iter().cloned());
    out
}

// ── Message ─────────────────────────────────────────────────────────────────

/// Display metadata for compact tool rows shown in chat.
#[derive(Debug, Clone)]
pub struct ToolDisplay {
    pub label: String,
    pub is_error: bool,
}

/// A single chat message.
#[derive(Debug, Clone)]
pub struct Message {
    pub role: ChatRole,
    pub content: String,
    /// Present when this message represents a tool call or result.
    pub tool: Option<ToolDisplay>,
}

impl Message {
    #[must_use]
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::User,
            content: content.into(),
            tool: None,
        }
    }

    #[must_use]
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::Assistant,
            content: content.into(),
            tool: None,
        }
    }

    #[must_use]
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::System,
            content: content.into(),
            tool: None,
        }
    }

    #[must_use]
    pub fn tool_row(label: String, is_error: bool) -> Self {
        Self {
            role: ChatRole::Tool,
            content: String::new(),
            tool: Some(ToolDisplay { label, is_error }),
        }
    }

    /// Terminal output: shows a label (e.g. "bash: ls") plus visible content.
    #[must_use]
    pub fn terminal_output(command: String, content: String, is_error: bool) -> Self {
        Self {
            role: ChatRole::Tool,
            content,
            tool: Some(ToolDisplay {
                label: format!("bash: {command}"),
                is_error,
            }),
        }
    }
}

//! Conversation model: on-disk message representation and provider chat-history assembly.

use crate::llm::{ChatMessage, ChatRole};

// ── History ─────────────────────────────────────────────────────────────────

/// Build provider history without truncating conversation or tool chains.
pub fn build_chat_history(messages: &[ChatMessage], system_prompt: &str) -> Vec<ChatMessage> {
    let mut out = Vec::with_capacity(messages.len() + 1);
    out.push(ChatMessage::new(ChatRole::System, system_prompt));
    out.extend(provider_facing_messages(messages));
    out
}

/// Clone messages and add the same provider-only timing context used by normal
/// conversation requests.
pub(crate) fn provider_facing_messages(messages: &[ChatMessage]) -> Vec<ChatMessage> {
    let mut out = Vec::with_capacity(messages.len());
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
    let timing = format!("<timing>{timing}</timing>");
    if message.content.ends_with(&timing) {
        return message;
    }
    if !message.content.is_empty() {
        message.content.push_str("\n\n");
    }
    message.content.push_str(&timing);
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
#[path = "chat_tests.rs"]
mod chat_tests;

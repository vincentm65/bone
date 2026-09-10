//! Conversation model: on-disk message representation and provider chat-history assembly.

use crate::llm::provider::ToolResult;
use crate::llm::{ChatMessage, ChatRole};

// ── History ─────────────────────────────────────────────────────────────────

/// Deterministic answer for a tool call whose result was never recorded (an
/// aborted, crashed, or interrupted turn).
const INTERRUPTED_TOOL_RESULT: &str = "Tool call was interrupted; no result was recorded.";

/// Repair malformed stored tool-call sequences so a wedged conversation becomes
/// sendable again. Two independent fixes run in a single pass:
///
/// * Image relays interleaved inside a tool batch are moved after the batch.
///   Strict providers require every `tool_call_id` to be answered immediately
///   by its tool message, so `assistant(tool_calls=[a,b]) → tool a → relay →
///   tool b → relay` is rejected; it is rewritten to `… → tool a → tool b →
///   relay a → relay b` without inventing or dropping data.
/// * A tool call whose reply is missing gains a deterministic error placeholder,
///   in call order, so an interrupted turn no longer dangles.
///
/// Idempotent: an already-valid sequence is returned unchanged, and running the
/// pass twice produces the same result.
pub fn repair_tool_call_sequences(messages: &mut Vec<ChatMessage>) {
    if !messages
        .iter()
        .any(|m| m.role == ChatRole::Assistant && !m.tool_calls.is_empty())
    {
        return;
    }

    let mut out: Vec<ChatMessage> = Vec::with_capacity(messages.len());
    let mut index = 0;
    while index < messages.len() {
        let message = messages[index].clone();
        if message.role != ChatRole::Assistant || message.tool_calls.is_empty() {
            out.push(message);
            index += 1;
            continue;
        }

        // Consume the batch's replies and any relays interleaved among them.
        // A reply must belong to this assistant; relays carry no call id.
        let expected = message.tool_calls.clone();
        let mut replies: std::collections::HashMap<String, ChatMessage> =
            std::collections::HashMap::new();
        let mut relays: Vec<ChatMessage> = Vec::new();
        let mut scan = index + 1;
        while scan < messages.len() {
            let candidate = &messages[scan];
            if candidate.role == ChatRole::Tool
                && let Some(id) = candidate.tool_call_id.as_deref()
                && expected.iter().any(|call| call.id == id)
            {
                if replies.contains_key(id) {
                    break;
                }
                replies.insert(id.to_string(), candidate.clone());
                scan += 1;
                continue;
            }
            if candidate.is_synthetic_relay() {
                relays.push(candidate.clone());
                scan += 1;
                continue;
            }
            break;
        }

        out.push(message);
        for call in expected {
            match replies.remove(&call.id) {
                Some(reply) => out.push(reply),
                None => out.push(ChatMessage::tool(ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    INTERRUPTED_TOOL_RESULT,
                ))),
            }
        }
        out.extend(relays);
        index = scan;
    }
    *messages = out;
}

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

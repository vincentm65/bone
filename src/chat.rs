use crate::llm::prompts;
use crate::llm::{ChatMessage, ChatRole};

// ── History ─────────────────────────────────────────────────────────────────

/// Build provider history without truncating conversation or tool chains.
/// If `custom_system_prompt` is provided, it replaces the default bone system prompt.
/// If `skills_catalog` is provided, it is appended to the system prompt.
pub fn build_chat_history(
    messages: &[ChatMessage],
    custom_system_prompt: Option<&str>,
    skills_catalog: &str,
) -> Vec<ChatMessage> {
    let mut out = Vec::with_capacity(messages.len() + 1);
    let mut system_content = match custom_system_prompt {
        Some(s) => s.to_string(),
        None => prompts::system_prompt(),
    };
    system_content.push_str(skills_catalog);
    out.push(ChatMessage::new(ChatRole::System, system_content));
    out.extend(messages.iter().cloned());
    out
}

/// Compute the compact boundary index for `messages` given `keep` count.
/// Returns `None` if the transcript is short enough that no compaction is needed.
pub fn find_compact_boundary(messages: &[ChatMessage], keep: usize) -> Option<usize> {
    let keep = keep.max(1);
    if messages.len() <= keep {
        return None;
    }
    let boundary = compact_boundary(messages, messages.len() - keep);
    if boundary == 0 { None } else { Some(boundary) }
}

/// Notice inserted when older messages are compacted.
pub const COMPACT_NOTICE: &str = "Compacted older messages.";

/// Default number of recent messages to keep during compaction.
pub const DEFAULT_KEEP_MESSAGES: usize = 12;

/// Build the messages to send to the LLM for a compaction summary.
/// Takes the older messages that will be discarded and wraps them with
/// a summary instruction system prompt.
pub fn build_summary_messages(old_messages: &[ChatMessage]) -> Vec<ChatMessage> {
    let mut out = Vec::with_capacity(old_messages.len() + 1);
    out.push(ChatMessage::new(
        ChatRole::System,
        crate::llm::prompts::compact_summary_prompt().to_string(),
    ));
    out.extend(old_messages.iter().cloned());
    out
}

fn compact_boundary(messages: &[ChatMessage], requested: usize) -> usize {
    let mut boundary = requested;

    // Walk backward until we find a safe cut point:
    //   - boundary must not point at a tool result (needs its assistant call)
    //   - message before boundary must not be a tool result (would orphan it)
    while boundary > 0 {
        if boundary < messages.len() && messages[boundary].role == ChatRole::Tool {
            boundary -= 1;
            continue;
        }
        if messages[boundary - 1].role == ChatRole::Tool {
            boundary -= 1;
            continue;
        }
        break;
    }

    // Include the assistant call that initiated a retained tool chain
    if boundary > 0
        && messages[boundary - 1].role == ChatRole::Assistant
        && !messages[boundary - 1].tool_calls.is_empty()
    {
        boundary -= 1;
    }

    boundary
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

    /// Terminal output: shows a label (e.g. "shell: ls") plus visible content.
    #[must_use]
    pub fn terminal_output(command: String, content: String, is_error: bool) -> Self {
        Self {
            role: ChatRole::Tool,
            content,
            tool: Some(ToolDisplay {
                label: format!("shell: {command}"),
                is_error,
            }),
        }
    }
}

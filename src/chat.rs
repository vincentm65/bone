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

/// Notice inserted when older messages are compacted.
pub const COMPACT_NOTICE: &str = "Compacted older messages.";

/// Default number of recent messages to keep during compaction.
pub const DEFAULT_KEEP_MESSAGES: usize = 12;

/// Compact chat transcript by replacing older turns with a short notice.
///
/// This does not summarize removed content; it only keeps the most recent
/// messages, expanding the retained range when needed to avoid splitting
/// assistant/tool-call chains.
///
/// Returns a `Cow` to avoid allocation when the transcript is already short
/// enough to keep as-is.
pub fn compact_transcript<'a>(
    messages: &'a [ChatMessage],
    keep: usize,
) -> std::borrow::Cow<'a, [ChatMessage]> {
    let keep = keep.max(1);
    if messages.len() <= keep {
        return std::borrow::Cow::Borrowed(messages);
    }

    let keep_from = compact_boundary(messages, messages.len() - keep);
    if keep_from == 0 {
        return std::borrow::Cow::Borrowed(messages);
    }

    let mut out = Vec::with_capacity(messages.len() - keep_from + 1);
    out.push(ChatMessage::new(ChatRole::System, COMPACT_NOTICE));
    out.extend(messages[keep_from..].iter().cloned());
    std::borrow::Cow::Owned(out)
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

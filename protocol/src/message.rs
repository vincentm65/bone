//! Chat message types that cross the frontend↔daemon wire.

use serde::{Deserialize, Serialize};

/// Provider-neutral chat roles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    System,
    User,
    Assistant,
    Tool,
}

impl ChatRole {
    pub fn as_str(self) -> &'static str {
        match self {
            ChatRole::System => "system",
            ChatRole::User => "user",
            ChatRole::Assistant => "assistant",
            ChatRole::Tool => "tool",
        }
    }
}

/// A single image attachment carried by a message, ready for the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageData {
    pub media_type: String,
    pub data: String,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// A tool call produced by the model or replayed in a message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// A completed tool result stored in a message.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub name: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImageData>,
    pub is_error: bool,
    #[serde(skip)]
    pub pane_page: Option<crate::view::PaneContent>,
    #[serde(skip)]
    pub state: Option<String>,
}

impl ToolResult {
    pub fn ok(
        call_id: impl Into<String>,
        name: impl Into<String>,
        output: crate::tools::ToolOutput,
    ) -> Self {
        Self {
            call_id: call_id.into(),
            name: name.into(),
            content: output.content,
            images: output.images,
            pane_page: output.pane_page,
            state: output.state,
            ..Default::default()
        }
    }

    pub fn error(
        call_id: impl Into<String>,
        name: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            call_id: call_id.into(),
            name: name.into(),
            content: content.into(),
            is_error: true,
            ..Default::default()
        }
    }
}

/// Provider-neutral reasoning/thinking content captured during a turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reasoning {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub echo_field: Option<String>,
}

/// An encrypted reasoning output item (OpenAI Responses API / Codex).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasoningItem {
    pub id: String,
    pub encrypted_content: String,
}

/// One item of an assistant turn's output, in emission order.
#[derive(Debug, Clone)]
pub enum OutputItem {
    Text(String),
    Reasoning(ReasoningItem),
    ToolCall(ToolCall),
}

/// Provider-neutral chat message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImageData>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_error: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Reasoning>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasoning_items: Vec<ReasoningItem>,
    #[serde(skip)]
    pub output_sequence: Vec<OutputItem>,
}

impl ChatMessage {
    pub fn new(role: ChatRole, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
            is_error: false,
            reasoning: None,
            reasoning_items: Vec::new(),
            output_sequence: Vec::new(),
        }
    }

    pub fn user_with_images(content: impl Into<String>, images: Vec<ImageData>) -> Self {
        Self {
            images,
            ..Self::new(ChatRole::User, content)
        }
    }

    pub fn assistant_with_tools(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            tool_calls,
            ..Self::new(ChatRole::Assistant, content)
        }
    }

    pub fn tool(result: ToolResult) -> Self {
        Self {
            role: ChatRole::Tool,
            content: result.content,
            images: result.images,
            tool_calls: Vec::new(),
            tool_call_id: Some(result.call_id),
            name: Some(result.name),
            is_error: result.is_error,
            reasoning: None,
            reasoning_items: Vec::new(),
            output_sequence: Vec::new(),
        }
    }
}

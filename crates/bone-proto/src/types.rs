//! Data types shared by methods and events.

use serde::{Deserialize, Serialize};

pub type SessionId = String;
pub type TurnId = u64;
pub type AskId = u64;

/// One entry in a session transcript. Provider-neutral; providers translate
/// to and from their own wire formats.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum ChatMessage {
    System {
        content: String,
    },
    User {
        content: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<ImageAttachment>,
    },
    Assistant {
        #[serde(default, skip_serializing_if = "String::is_empty")]
        content: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        reasoning: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ToolCall>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_state: Option<ReasoningState>,
    },
    Tool {
        call_id: String,
        content: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<ImageAttachment>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
    },
}

impl ChatMessage {
    pub fn images(&self) -> &[ImageAttachment] {
        match self {
            Self::User { images, .. } | Self::Tool { images, .. } => images,
            _ => &[],
        }
    }

    pub fn images_mut(&mut self) -> Option<&mut Vec<ImageAttachment>> {
        match self {
            Self::User { images, .. } | Self::Tool { images, .. } => Some(images),
            _ => None,
        }
    }
}

/// Reasoning a provider returns sealed (OpenAI's encrypted reasoning items),
/// replayed on later calls so the model keeps its thinking. Only the model
/// that wrote it can read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningState {
    pub model: String,
    pub items: Vec<serde_json::Value>,
    /// A fingerprint of the conversation the reasoning was produced in, for
    /// providers that only accept it back after an unchanged history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    /// The Chat Completions field the reasoning streamed in
    /// (`reasoning_content` or `reasoning`), to replay it in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

/// The wire format of a built-in provider.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderApi {
    /// `/chat/completions`.
    #[default]
    Completions,
    /// `/responses`, which keeps OpenAI models' encrypted reasoning.
    Responses,
    /// Anthropic's `/messages`, with prompt caching and thinking.
    Anthropic,
}

impl ProviderApi {
    pub fn is_completions(&self) -> bool {
        *self == Self::Completions
    }
}

/// A tool call requested by the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Raw JSON arguments exactly as the model produced them. May be invalid
    /// JSON; the tool reports that back to the model as an error.
    pub arguments: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u64>,
    /// Input context of the last model call, distinct from aggregate consumption.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session_id: SessionId,
    pub cwd: String,
    /// Unix seconds.
    pub created_at: u64,
    /// Set with `session/rename`, else the first user message, truncated;
    /// `None` until there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The session this one was forked from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<SessionId>,
    /// The session (and tool call) that started this one as a subagent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<SessionOwner>,
}

/// Who started a session on its behalf: a tool call in another session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionOwner {
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    /// A short name for it (the subagent's), for UIs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeltaKind {
    Text,
    Reasoning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum TurnOutcome {
    Completed,
    Cancelled,
    Failed { message: String },
}

/// A durable image reference. `data` is hydrated only for provider calls;
/// transcripts, queues and events keep the small reference without pixels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageAttachment {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub width: u32,
    pub height: u32,
    pub bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
}

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
    },
    Assistant {
        #[serde(default, skip_serializing_if = "String::is_empty")]
        content: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        reasoning: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ToolCall>,
    },
    Tool {
        call_id: String,
        content: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
    },
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

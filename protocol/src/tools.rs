//! Tool-call approval decision for the wire protocol.

use serde::{Deserialize, Serialize};

use crate::message::ImageData;
use crate::view::PaneContent;

/// Outcome of deciding whether a single tool call may execute.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallOutcome {
    Approve,
    Blocked(String),
    Denied,
}

/// A tool definition sent to the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// Output produced by a tool execution.
#[derive(Debug, Clone, Default)]
pub struct ToolOutput {
    pub content: String,
    pub images: Vec<ImageData>,
    pub pane_page: Option<PaneContent>,
    pub state: Option<String>,
}

impl ToolOutput {
    pub fn text(content: String) -> Self {
        Self {
            content,
            ..Default::default()
        }
    }

    pub fn with_images(content: String, images: Vec<ImageData>) -> Self {
        Self {
            content,
            images,
            ..Default::default()
        }
    }
}

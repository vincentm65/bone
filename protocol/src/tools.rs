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
    /// Optional display-only preview for a successful edit. The model-facing
    /// tool content remains in `content`.
    pub edit_preview: Option<String>,
    pub images: Vec<ImageData>,
    /// Keep image data only in the current provider tool loop. Ephemeral images
    /// are never added to durable transcript/session history.
    pub ephemeral_images: bool,
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

/// Per-tool transcript presentation declared by the tool (label template,
/// argument labels, and result visibility).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolDisplayConfig {
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub template: Option<String>,
    #[serde(default)]
    pub value_labels: std::collections::HashMap<String, std::collections::HashMap<String, String>>,
    #[serde(default)]
    pub show: Option<bool>,
    #[serde(default)]
    pub show_result: Option<bool>,
    #[serde(default)]
    pub eager: Option<bool>,
}

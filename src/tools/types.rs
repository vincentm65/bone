use crate::ui::pane_page::PanePage;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

// Re-export types moved to dedicated modules so existing imports keep working.
pub use crate::tools::command_policy::{CommandSafety, classify_command};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub name: String,
    pub content: String,
    pub is_error: bool,
    /// Optional pane page to display in the bottom pane.
    /// Not serialized — this is a UI-only field.
    #[serde(skip)]
    pub pane_page: Option<PanePage>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolDisplayConfig {
    /// Argument names to show in the compact tool-call row.
    #[serde(default)]
    pub args: Vec<String>,
    /// Optional simple template. Placeholders like `{query}` are replaced
    /// with argument values when present.
    #[serde(default)]
    pub template: Option<String>,
    /// Whether to show the tool call row in chat. Defaults to true.
    #[serde(default)]
    pub show: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub content: String,
    pub pane_page: Option<PanePage>,
}

#[derive(Debug, Clone, Default)]
pub struct ToolExecutionContext {
    pub call_id: String,
}

#[derive(Debug, Clone)]
pub enum ToolLiveEvent {
    Pane(PanePage),
}

impl ToolOutput {
    pub fn text(content: String) -> Self {
        Self {
            content,
            pane_page: None,
        }
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn definition(&self) -> ToolDefinition;
    async fn execute(&self, arguments: Value) -> Result<String, String>;

    async fn execute_output(&self, arguments: Value) -> Result<ToolOutput, String> {
        self.execute(arguments).await.map(ToolOutput::text)
    }

    async fn execute_output_live(
        &self,
        arguments: Value,
        _events: Option<tokio::sync::mpsc::UnboundedSender<ToolLiveEvent>>,
        _context: ToolExecutionContext,
    ) -> Result<ToolOutput, String> {
        self.execute_output(arguments).await
    }
}

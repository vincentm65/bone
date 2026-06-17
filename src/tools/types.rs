use crate::pane_content::{KeyRequest, PaneContent};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    /// Optional pane content to display in the bottom pane.
    /// Not serialized — this is a UI-only field.
    #[serde(skip)]
    pub pane_page: Option<PaneContent>,
    /// Optional session state to store in ToolStateMap (not sent to LLM).
    #[serde(skip)]
    pub state: Option<String>,
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
    /// Whether to show the tool result content in chat. Defaults to false.
    #[serde(default)]
    pub show_result: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub content: String,
    pub pane_page: Option<PaneContent>,
    /// Optional session state to store in ToolStateMap (not sent to LLM).
    pub state: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ToolExecutionContext {
    pub call_id: String,
    /// Session state previously stored by this tool, injected as TOOL_SESSION_STATE.
    pub session_state: Option<String>,
    pub owner: String,
    /// Cancellation flag set by the TUI when the user cancels streaming.
    pub cancelled: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// Nesting depth of subagent calls (0 = top-level).
    pub agent_depth: usize,
    /// Nesting depth of Lua ctx.tools.call delegation (0 = top-level tool call).
    pub tool_call_depth: usize,
    /// Tool handler for ctx.tools.* delegation (set by ToolHandler).
    pub tool_handler: Option<crate::tools::registry::ToolHandler>,
    /// App-derived ctx snapshot (session/provider/model/usage/history), so tools
    /// see the same `ctx` as slash commands. Set by ToolHandler; `None` for
    /// non-live calls.
    pub(crate) app_state: Option<crate::ext::ctx::AppCtxState>,
}

#[derive(Debug)]
pub enum ToolLiveEvent {
    /// Request the next terminal key; block until `reply` resolves.
    Key(KeyRequest),
}

impl ToolOutput {
    pub fn text(content: String) -> Self {
        Self {
            content,
            pane_page: None,
            state: None,
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

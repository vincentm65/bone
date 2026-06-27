//! Tool trait, definitions, calls, results, and execution-context types.
//!
//! Wire-format types are re-exported from `bone-protocol`; only
//! non-wire types (`ToolDisplayConfig`, `ToolExecutionContext`,
//! `ToolLiveEvent`, `Tool`) stay core-local.

use crate::pane_content::KeyRequest;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

// Re-export wire-format types from protocol.
pub use bone_protocol::{ToolCall, ToolDefinition, ToolOutput, ToolResult};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolDisplayConfig {
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub template: Option<String>,
    #[serde(default)]
    pub show: Option<bool>,
    #[serde(default)]
    pub show_result: Option<bool>,
    #[serde(default)]
    pub eager: Option<bool>,
}

#[derive(Debug, Clone, Default)]
pub struct ToolExecutionContext {
    pub call_id: String,
    pub session_state: Option<String>,
    pub owner: String,
    pub cancelled: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    pub agent_depth: usize,
    pub tool_call_depth: usize,
    pub tool_handler: Option<crate::tools::registry::ToolHandler>,
    pub(crate) app_state: Option<crate::ext::ctx::AppCtxState>,
}

#[derive(Debug)]
pub enum ToolLiveEvent {
    Key(KeyRequest),
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

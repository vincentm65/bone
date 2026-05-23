use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

// Re-export types moved to dedicated modules so existing imports keep working.
pub use crate::tools::approval::ApprovalMode;
pub use crate::tools::command_policy::{
    CommandSafety, is_dangerous_git_bash_call, minimum_required_classification,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: &'static str,
    pub description: &'static str,
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
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn definition(&self) -> ToolDefinition;
    async fn execute(&self, arguments: Value) -> Result<String, String>;
}

use crate::tools::registry::ToolRegistry;
use crate::tools::types::{ToolCall, ToolDefinition, ToolResult};

pub struct ToolHandler {
    registry: ToolRegistry,
}

impl ToolHandler {
    pub fn new(registry: ToolRegistry) -> Self {
        Self { registry }
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.registry.definitions()
    }

    pub async fn execute_all(&self, calls: Vec<ToolCall>) -> Vec<ToolResult> {
        let mut results = Vec::with_capacity(calls.len());
        for call in calls {
            results.push(self.registry.execute(call).await);
        }
        results
    }
}

use std::collections::HashMap;
use std::sync::Arc;

use crate::tools::types::{Tool, ToolCall, ToolDefinition, ToolResult};

pub struct ToolRegistry {
    tools: HashMap<&'static str, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self { tools: HashMap::new() }
    }

    pub fn register<T: Tool + 'static>(mut self, tool: T) -> Self {
        let name = tool.definition().name;
        self.tools.insert(name, Arc::new(tool));
        self
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools.values().map(|tool| tool.definition()).collect()
    }

    pub async fn execute(&self, call: ToolCall) -> ToolResult {
        let name = call.name.clone();
        match self.tools.get(name.as_str()) {
            Some(tool) => match tool.execute(call.arguments).await {
                Ok(content) => ToolResult { call_id: call.id, name, content, is_error: false },
                Err(content) => ToolResult { call_id: call.id, name, content, is_error: true },
            },
            None => ToolResult {
                call_id: call.id,
                name,
                content: "Unknown tool".to_string(),
                is_error: true,
            },
        }
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

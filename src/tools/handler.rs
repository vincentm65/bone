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
        let n = calls.len();
        let registry = self.registry.clone();
        let tasks: Vec<_> = calls
            .into_iter()
            .enumerate()
            .map(|(i, call)| {
                let reg = registry.clone();
                tokio::spawn(async move { (i, reg.execute(call).await) })
            })
            .collect();

        let mut results: Vec<Option<ToolResult>> = vec![None; n];
        for handle in tasks {
            match handle.await {
                Ok((i, result)) => {
                    if i < n {
                        results[i] = Some(result);
                    }
                }
                Err(_) => {
                    // Task panicked — we'll fill remaining slots below.
                }
            }
        }

        results
            .into_iter()
            .enumerate()
            .map(|(i, r)| {
                r.unwrap_or_else(|| ToolResult {
                    call_id: String::new(),
                    name: format!("slot-{i}"),
                    content: "internal error: tool execution task failed".into(),
                    is_error: true,
                })
            })
            .collect()
    }
}

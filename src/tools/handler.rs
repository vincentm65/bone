use crate::tools::registry::ToolRegistry;
use crate::tools::types::{ToolCall, ToolDefinition, ToolResult};
use futures_util::future::join_all;

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
        join_all(calls.into_iter().map(|call| self.registry.execute(call))).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::types::{Tool, ToolDefinition};
    use async_trait::async_trait;
    use serde_json::{Value, json};
    use std::sync::{Arc, Mutex};
    use tokio::time::{Duration, sleep};

    struct RecordingTool {
        order: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl Tool for RecordingTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: "record",
                description: "record execution order",
                input_schema: json!({ "type": "object" }),
            }
        }

        async fn execute(&self, arguments: Value) -> Result<String, String> {
            let value = arguments["value"].as_str().unwrap().to_string();
            let delay_ms = arguments["delay_ms"].as_u64().unwrap_or(0);
            sleep(Duration::from_millis(delay_ms)).await;
            self.order.lock().unwrap().push(value.clone());
            Ok(value)
        }
    }

    #[tokio::test]
    async fn execute_all_returns_results_in_request_order_after_concurrent_execution() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let handler = ToolHandler::new(ToolRegistry::new().register(RecordingTool {
            order: order.clone(),
        }));

        let results = handler
            .execute_all(vec![
                ToolCall {
                    id: "slow".into(),
                    name: "record".into(),
                    arguments: json!({ "value": "first", "delay_ms": 20 }),
                },
                ToolCall {
                    id: "fast".into(),
                    name: "record".into(),
                    arguments: json!({ "value": "second", "delay_ms": 0 }),
                },
            ])
            .await;

        assert_eq!(*order.lock().unwrap(), vec!["second", "first"]);
        assert_eq!(results[0].call_id, "slow");
        assert_eq!(results[1].call_id, "fast");
    }
}

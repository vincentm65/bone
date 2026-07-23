use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::Notify;

use super::{ToolHandler, ToolRegistry};
use crate::runtime::RuntimeEvent;
use crate::tools::types::{Tool, ToolCall, ToolDefinition};

struct GateTool {
    release: Arc<Notify>,
}

#[async_trait]
impl Tool for GateTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "gate".into(),
            description: "test tool".into(),
            input_schema: json!({"type": "object"}),
        }
    }

    async fn execute(&self, arguments: Value) -> Result<String, String> {
        let label = arguments["label"].as_str().unwrap().to_string();
        if arguments["wait"].as_bool().unwrap_or(false) {
            self.release.notified().await;
        }
        Ok(label)
    }
}

fn call(id: &str, label: &str, wait: bool) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: "gate".into(),
        arguments: json!({"label": label, "wait": wait}),
    }
}

#[tokio::test]
async fn top_level_results_emit_as_each_parallel_call_finishes() {
    let release = Arc::new(Notify::new());
    let mut registry = ToolRegistry::new();
    registry.register_mut(GateTool {
        release: release.clone(),
    });
    let handler = ToolHandler::new(registry);
    let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();

    let execution = tokio::spawn(async move {
        handler
            .execute_all_live(
                vec![call("slow", "slow", true), call("fast", "fast", false)],
                None,
                0,
                0,
                Some(events_tx),
            )
            .await
    });

    let first = tokio::time::timeout(Duration::from_secs(1), events_rx.recv())
        .await
        .expect("fast result event timed out")
        .expect("event channel closed");
    assert!(matches!(
        first,
        RuntimeEvent::ToolResult { call_id, content, .. }
            if call_id == "fast" && content == "fast"
    ));
    assert!(!execution.is_finished());

    release.notify_waiters();
    let second = tokio::time::timeout(Duration::from_secs(1), events_rx.recv())
        .await
        .expect("slow result event timed out")
        .expect("event channel closed");
    assert!(matches!(
        second,
        RuntimeEvent::ToolResult { call_id, content, .. }
            if call_id == "slow" && content == "slow"
    ));

    let results = execution.await.expect("tool execution task panicked");
    assert_eq!(
        results
            .iter()
            .map(|result| result.call_id.as_str())
            .collect::<Vec<_>>(),
        ["slow", "fast"]
    );
    assert!(events_rx.try_recv().is_err());
}

#[tokio::test]
async fn nested_tool_results_do_not_emit_top_level_rows() {
    let mut registry = ToolRegistry::new();
    registry.register_mut(GateTool {
        release: Arc::new(Notify::new()),
    });
    let handler = ToolHandler::new(registry);
    let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();

    let results = handler
        .execute_all_live(
            vec![call("nested", "nested", false)],
            None,
            0,
            1,
            Some(events_tx),
        )
        .await;

    assert_eq!(results[0].content, "nested");
    assert!(events_rx.try_recv().is_err());
}

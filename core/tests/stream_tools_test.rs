use async_trait::async_trait;
use bone_core::tools::ApprovalMode;
use bone_core::tools::registry::{ToolHandler, ToolRegistry};
use bone_core::tools::types::{Tool, ToolCall, ToolDefinition, ToolLiveEvent, ToolOutput};
use serde_json::{Value, json};
use std::collections::HashMap;
use tokio::sync::mpsc;

struct MockTool {
    name: String,
    result: Result<String, String>,
}

#[async_trait]
impl Tool for MockTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.clone(),
            description: format!("mock {}", self.name),
            input_schema: json!({"type": "object", "properties": {}}),
        }
    }

    async fn execute(&self, _arguments: Value) -> Result<String, String> {
        self.result.clone()
    }
}

struct SlowTool {
    name: String,
    delay_ms: u64,
    content: String,
}

#[async_trait]
impl Tool for SlowTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.clone(),
            description: format!("slow {}", self.name),
            input_schema: json!({"type": "object", "properties": {}}),
        }
    }

    async fn execute(&self, _arguments: Value) -> Result<String, String> {
        tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
        Ok(self.content.clone())
    }
}

struct PaneTool;

#[async_trait]
impl Tool for PaneTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "pane_tool".to_string(),
            description: "emits a pane page".to_string(),
            input_schema: json!({"type": "object", "properties": {}}),
        }
    }

    async fn execute(&self, _arguments: Value) -> Result<String, String> {
        Ok("pane result".to_string())
    }

    async fn execute_output(&self, _arguments: Value) -> Result<ToolOutput, String> {
        let pane = bone_core::pane_content::PaneContent {
            source: "test-pane".to_string(),
            title: "Test Pane".to_string(),
            lines: vec![
                bone_core::pane_content::PaneLineSpec::Plain("line 1".to_string()),
                bone_core::pane_content::PaneLineSpec::Plain("line 2".to_string()),
            ],
            visible_rows: 4,
            scroll: 0,
        };
        Ok(ToolOutput {
            content: "pane result".to_string(),
            images: Vec::new(),
            pane_page: Some(pane),
            state: None,
        })
    }
}

fn make_call(name: &str, id: &str) -> ToolCall {
    ToolCall {
        id: id.to_string(),
        name: name.to_string(),
        arguments: json!({}),
    }
}

#[tokio::test]
async fn tool_handler_execute_all_returns_results_in_order() {
    let registry = ToolRegistry::new()
        .register(MockTool {
            name: "tool_a".to_string(),
            result: Ok("result_a".to_string()),
        })
        .register(MockTool {
            name: "tool_b".to_string(),
            result: Ok("result_b".to_string()),
        });

    let handler = ToolHandler::new(registry);
    let calls = vec![make_call("tool_a", "c1"), make_call("tool_b", "c2")];

    let results = handler.execute_all(calls, 0).await;
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].content, "result_a");
    assert!(!results[0].is_error);
    assert_eq!(results[1].content, "result_b");
    assert!(!results[1].is_error);
}

#[tokio::test]
async fn tool_handler_execute_all_reports_errors() {
    let registry = ToolRegistry::new().register(MockTool {
        name: "failing".to_string(),
        result: Err("something broke".to_string()),
    });

    let handler = ToolHandler::new(registry);
    let calls = vec![make_call("failing", "c1")];

    let results = handler.execute_all(calls, 0).await;
    assert!(results[0].is_error);
    assert_eq!(results[0].content, "something broke");
}

#[tokio::test]
async fn tool_handler_execute_all_unknown_tool() {
    let registry = ToolRegistry::new();
    let handler = ToolHandler::new(registry);
    let calls = vec![make_call("nonexistent", "c1")];

    let results = handler.execute_all(calls, 0).await;
    assert!(results[0].is_error);
    assert_eq!(results[0].content, "Tool disabled in /tools settings");
}

#[tokio::test]
async fn slash_named_tools_are_not_advertised_or_executed_as_commands() {
    let handler = ToolHandler::new(ToolRegistry::new().register(MockTool {
        name: "/history".to_string(),
        result: Ok("should not run".to_string()),
    }));

    assert!(handler.definitions().is_empty());
    let results = handler
        .execute_all(vec![make_call("/history", "c1")], 0)
        .await;
    assert!(results[0].is_error);
    assert_eq!(
        results[0].content,
        "Slash commands are UI commands, not tools."
    );
}

#[tokio::test]
async fn tool_handler_execute_all_disabled_tool() {
    let registry = ToolRegistry::new().register(MockTool {
        name: "disabled_tool".to_string(),
        result: Ok("ok".to_string()),
    });

    let handler = ToolHandler::with_enabled_safety_and_display(
        registry,
        &[],
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
    );
    let calls = vec![make_call("disabled_tool", "c1")];

    let results = handler.execute_all(calls, 0).await;
    assert!(results[0].is_error);
    assert!(results[0].content.contains("disabled"));
}

#[tokio::test]
async fn tool_handler_execute_all_runs_in_parallel() {
    let registry = ToolRegistry::new()
        .register(SlowTool {
            name: "slow_a".to_string(),
            delay_ms: 100,
            content: "a".to_string(),
        })
        .register(SlowTool {
            name: "slow_b".to_string(),
            delay_ms: 100,
            content: "b".to_string(),
        });

    let handler = ToolHandler::new(registry);
    let calls = vec![make_call("slow_a", "c1"), make_call("slow_b", "c2")];

    let start = std::time::Instant::now();
    let results = handler.execute_all(calls, 0).await;
    let elapsed = start.elapsed();

    assert_eq!(results.len(), 2);
    assert!(
        elapsed < std::time::Duration::from_millis(300),
        "execute_all should run tools concurrently, took {:?}",
        elapsed
    );
}

#[tokio::test]
async fn tool_handler_execute_live_receives_pane_events() {
    let registry = ToolRegistry::new().register(PaneTool);

    let handler = ToolHandler::new(registry);
    let calls = vec![make_call("pane_tool", "c1")];

    let (tx, _rx) = mpsc::unbounded_channel::<ToolLiveEvent>();
    let results = handler.execute_all_live(calls, Some(tx), 0, 0).await;

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].content, "pane result");
    assert!(results[0].pane_page.is_some());
    let pane = results[0].pane_page.as_ref().unwrap();
    assert_eq!(pane.source, "test-pane");
}

#[tokio::test]
async fn tool_handler_execute_live_no_events_channel() {
    let registry = ToolRegistry::new().register(MockTool {
        name: "simple".to_string(),
        result: Ok("ok".to_string()),
    });

    let handler = ToolHandler::new(registry);
    let calls = vec![make_call("simple", "c1")];

    let results = handler.execute_all_live(calls, None, 0, 0).await;
    assert_eq!(results[0].content, "ok");
    assert!(!results[0].is_error);
}

#[tokio::test]
async fn tool_handler_allows_call_based_on_approval_mode() {
    let registry = ToolRegistry::new().register(MockTool {
        name: "read_file".to_string(),
        result: Ok("ok".to_string()),
    });

    let handler = ToolHandler::new(registry);
    let call = make_call("read_file", "c1");

    assert!(handler.allows_call(ApprovalMode::Safe, &call));
    assert!(handler.allows_call(ApprovalMode::Danger, &call));
}

#[test]
fn approval_mode_cycles() {
    assert!(matches!(ApprovalMode::Safe.cycle(), ApprovalMode::Danger));
    assert!(matches!(ApprovalMode::Danger.cycle(), ApprovalMode::Safe));
}

#[test]
fn approval_mode_labels() {
    assert_eq!(ApprovalMode::Safe.label(), "Safe");
    assert_eq!(ApprovalMode::Danger.label(), "Danger");
}

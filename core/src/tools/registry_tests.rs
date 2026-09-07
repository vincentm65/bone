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

struct SchemaTool {
    schema: Value,
}

#[async_trait]
impl Tool for SchemaTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "schema".into(),
            description: "test tool".into(),
            input_schema: self.schema.clone(),
        }
    }

    async fn execute(&self, _arguments: Value) -> Result<String, String> {
        Ok("ok".into())
    }
}

fn anyof_tool(variants: Value) -> SchemaTool {
    SchemaTool {
        schema: json!({ "anyOf": variants }),
    }
}

#[test]
fn required_fields_unions_anyof_variants_deduped_in_schema_order() {
    let tool = anyof_tool(json!([
        { "required": ["question", "options"] },
        { "required": ["question", "questions"] },
    ]));
    assert_eq!(
        super::required_fields(&tool),
        vec!["question", "options", "questions"]
    );
}

#[test]
fn empty_object_is_rejected_when_every_anyof_variant_requires_fields() {
    // ask_user-style schema: every `anyOf` branch requires fields, so `{}` can
    // match no branch and the guard must reject it (previously it slipped past).
    let tool = anyof_tool(json!([
        { "required": ["question", "options"] },
        { "required": ["questions"] },
    ]));
    let msg = super::reject_degenerate_arguments(&tool, &json!({}))
        .expect("empty object must be rejected");
    assert!(msg.contains("empty arguments object"), "unexpected: {msg}");
    assert!(
        msg.contains("question") && msg.contains("options") && msg.contains("questions"),
        "should name required fields: {msg}"
    );
}

#[test]
fn populated_object_matching_a_variant_is_not_rejected_by_the_guard() {
    let tool = anyof_tool(json!([
        { "required": ["question", "options"] },
        { "required": ["questions"] },
    ]));
    assert!(
        super::reject_degenerate_arguments(&tool, &json!({ "question": "hi", "options": ["a"] }))
            .is_none()
    );
    assert!(
        super::reject_degenerate_arguments(&tool, &json!({ "questions": [{ "question": "hi" }] }))
            .is_none()
    );
}

#[test]
fn empty_object_is_accepted_when_an_anyof_variant_has_no_required_fields() {
    // A variant with no `required` can be satisfied by `{}`, so the guard must
    // not treat an empty object as degenerate (avoids a false positive).
    let tool = anyof_tool(json!([
        { "required": ["question"] },
        { "type": "object" },
    ]));
    assert!(
        super::reject_degenerate_arguments(&tool, &json!({})).is_none(),
        "an empty object satisfies the variant with no required fields"
    );
}

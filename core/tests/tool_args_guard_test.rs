mod common;

use serde_json::{Value, json};

use bone_core::tools::registry::ToolHandler;
use bone_core::tools::{ToolCall, builtin_tools};

fn call(name: &str, arguments: Value) -> ToolCall {
    ToolCall {
        id: "call-1".to_string(),
        name: name.to_string(),
        arguments,
    }
}

async fn run_one(tool_call: ToolCall) -> bone_core::tools::ToolResult {
    let handler = ToolHandler::new(builtin_tools());
    handler
        .execute_all(vec![tool_call], 0)
        .await
        .into_iter()
        .next()
        .expect("one result")
}

#[tokio::test]
async fn null_arguments_are_rejected_with_required_fields() {
    let result = run_one(call("write_file", Value::Null)).await;

    assert!(result.is_error);
    assert!(
        result.content.contains("no arguments"),
        "unexpected: {}",
        result.content
    );
    assert!(
        result.content.contains("path") && result.content.contains("content"),
        "should name required fields: {}",
        result.content
    );
}

#[tokio::test]
async fn empty_object_arguments_are_rejected_with_required_fields() {
    let result = run_one(call("edit_file", json!({}))).await;

    assert!(result.is_error);
    assert!(
        result.content.contains("empty arguments object"),
        "unexpected: {}",
        result.content
    );
    assert!(
        result.content.contains("path")
            && result.content.contains("old_text")
            && result.content.contains("new_text"),
        "should name required fields: {}",
        result.content
    );
}

#[tokio::test]
async fn non_object_arguments_are_rejected() {
    let result = run_one(call("shell", json!("ls -la"))).await;

    assert!(result.is_error);
    assert!(
        result.content.contains("type string"),
        "unexpected: {}",
        result.content
    );
}

#[tokio::test]
async fn truncated_json_arguments_are_reported_as_truncation() {
    // Arguments cut off at the output-token cap arrive wrapped in a valid marker
    // object (see openai_compat flush_partial_tool_calls). The guard must flag
    // truncation and steer away from an identical retry — not mislabel it as
    // "no arguments" or accept the marker object as real arguments.
    let result = run_one(call(
        "edit_file",
        json!({
            bone_core::tools::TRUNCATED_ARGS_KEY: "{\"path\":\"world.rs\",\"content\":\"use std",
        }),
    ))
    .await;

    assert!(result.is_error);
    assert!(
        result.content.contains("truncated") && result.content.contains("do not resend"),
        "unexpected: {}",
        result.content
    );
    assert!(
        !result.content.contains("no arguments"),
        "should not mislabel truncation: {}",
        result.content
    );
}

#[tokio::test]
async fn populated_arguments_still_reach_the_tool() {
    let result = run_one(call(
        "edit_file",
        json!({ "path": "/nonexistent-guard-test" }),
    ))
    .await;

    // Reaches the tool itself (fails on the missing required edit fields,
    // not on the argument guard).
    assert!(result.is_error);
    assert!(
        !result.content.contains("re-send the call"),
        "guard should not fire: {}",
        result.content
    );
}

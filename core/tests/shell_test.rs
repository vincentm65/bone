mod common;

use serde_json::json;

use bone_core::tools::shell::ShellTool;
use bone_core::tools::shell::truncate_output;
use bone_core::tools::types::Tool;

#[tokio::test]
async fn timeout_returns_partial_stdout() {
    // A command that prints to stdout then sleeps past the timeout. The model
    // must still receive the partial output so it can decide next steps.
    let tool = ShellTool;

    let result = tool
        .execute(json!({
            "command": "echo 'partial compiler output' && sleep 5",
            "timeout_ms": 1000
        }))
        .await;

    assert!(result.is_err(), "expected timeout error, got: {result:?}");
    let err = result.unwrap_err();
    assert!(err.contains("[timed out after 1000ms; partial output]"));
    assert!(err.contains("partial compiler output"));
}

#[tokio::test]
async fn successful_command_returns_exit_code_and_stdout() {
    let tool = ShellTool;

    let result = tool
        .execute(json!({ "command": "echo hello" }))
        .await
        .expect("command should succeed");

    assert!(result.contains("exit code: 0"));
    assert!(result.contains("hello"));
}

#[test]
fn truncates_single_huge_line() {
    let output = truncate_output(&"x".repeat(10 * 1024 * 1024), 500);
    assert!(output.contains("…[truncated]"));
    assert!(output.len() < 10_000, "output len was {}", output.len());
}

#[tokio::test]
async fn classification_is_accepted_but_ignored() {
    // Old transcripts/providers still send classification; the schema no longer
    // requires it but the arg must still deserialize without error.
    let tool = ShellTool;

    let result = tool
        .execute(json!({
            "command": "echo ok",
            "classification": "read_only"
        }))
        .await
        .expect("command should succeed");

    assert!(result.contains("exit code: 0"));
}

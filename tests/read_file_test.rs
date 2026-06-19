mod common;

use std::path::PathBuf;

use serde_json::json;
use tokio::fs;

use bone::tools::read_file::ReadFileTool;
use bone::tools::types::Tool;

fn temp_path(name: &str) -> PathBuf {
    common::temp_path(&format!("read-file-{name}"))
}

#[tokio::test]
async fn reads_entire_file_when_no_options_given() {
    let path = temp_path("full.txt");
    fs::write(&path, "line one\nline two\nline three")
        .await
        .expect("setup");
    let tool = ReadFileTool;

    let result = tool
        .execute(json!({ "path": path }))
        .await
        .expect("read should succeed");

    assert_eq!(result, "line one\nline two\nline three");
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn start_line_skips_to_given_line() {
    let path = temp_path("start-line.txt");
    fs::write(&path, "alpha\nbeta\ngamma\ndelta")
        .await
        .expect("setup");
    let tool = ReadFileTool;

    let result = tool
        .execute(json!({ "path": path, "start_line": 3 }))
        .await
        .expect("read should succeed");

    assert_eq!(result, "gamma\ndelta");
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn max_lines_limits_output() {
    let path = temp_path("max-lines.txt");
    fs::write(&path, "a\nb\nc\nd\ne").await.expect("setup");
    let tool = ReadFileTool;

    let result = tool
        .execute(json!({ "path": path, "start_line": 2, "max_lines": 2 }))
        .await
        .expect("read should succeed");

    assert_eq!(result, "b\nc");
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn start_line_beyond_file_returns_empty() {
    let path = temp_path("beyond.txt");
    fs::write(&path, "only one line").await.expect("setup");
    let tool = ReadFileTool;

    let result = tool
        .execute(json!({ "path": path, "start_line": 99 }))
        .await
        .expect("read should succeed");

    assert_eq!(result, "");
    let _ = fs::remove_file(&path).await;
}

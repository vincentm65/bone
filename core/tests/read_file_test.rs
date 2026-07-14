mod common;

use std::path::PathBuf;

use bone_core::tools::read_file::ReadFileTool;
use bone_core::tools::types::{Tool, ToolExecutionContext};
use serde_json::json;
use tokio::fs;

fn temp_path(name: &str) -> PathBuf {
    common::temp_path(&format!("simple-read-{name}"))
}

#[tokio::test]
async fn returns_resolved_path_range_and_numbered_text() {
    let path = temp_path("full.txt");
    fs::write(&path, "line one\nline two\nline three")
        .await
        .unwrap();
    let result = ReadFileTool.execute(json!({ "path": path })).await.unwrap();
    let canonical = fs::canonicalize(&path).await.unwrap();
    assert!(result.starts_with(&format!("File: {}", canonical.display())));
    assert!(result.contains("Range: lines 1-3 of 3; entire file."));
    assert!(result.contains("    1 | line one"));
    assert!(result.contains("    3 | line three"));
    assert!(!result.contains('#'));
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn range_is_clear_and_provides_the_next_call() {
    let path = temp_path("range.txt");
    fs::write(&path, "a\nb\nc\nd").await.unwrap();
    let result = ReadFileTool
        .execute(json!({ "path": path, "start_line": 2, "max_lines": 2 }))
        .await
        .unwrap();
    assert!(result.contains("Range: lines 2-3 of 4"));
    assert!(result.contains("start_line=4"));
    assert!(result.contains("    2 | b"));
    assert!(!result.contains("    4 | d"));
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn start_beyond_eof_and_empty_files_are_explicit() {
    let path = temp_path("empty-range.txt");
    fs::write(&path, "only").await.unwrap();
    let beyond = ReadFileTool
        .execute(json!({ "path": path, "start_line": 99 }))
        .await
        .unwrap();
    assert!(beyond.contains("Range: no lines; file has 1 line"));
    fs::write(&path, "").await.unwrap();
    let empty = ReadFileTool.execute(json!({ "path": path })).await.unwrap();
    assert!(empty.contains("Range: empty file; 0 lines total"));
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn long_lines_are_bounded_and_marked_uneditable() {
    let path = temp_path("long.txt");
    fs::write(&path, "x".repeat(5000)).await.unwrap();
    let result = ReadFileTool.execute(json!({ "path": path })).await.unwrap();
    assert!(result.contains("…[truncated]"));
    assert!(result.contains("not editable"));
    assert!(result.len() < 5000);
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn image_files_still_return_attachments() {
    let dir = common::temp_dir("simple-read-image");
    fs::create_dir_all(&dir).await.unwrap();
    let path = dir.join("image.png");
    fs::write(&path, [137, 80, 78, 71]).await.unwrap();
    let result = ReadFileTool
        .execute_output(json!({ "path": path }))
        .await
        .unwrap();
    assert_eq!(result.images.len(), 1);
    assert_eq!(result.images[0].media_type, "image/png");
    let _ = fs::remove_dir_all(dir).await;
}

#[tokio::test]
async fn invalid_utf8_gets_an_actionable_error() {
    let path = temp_path("binary.bin");
    fs::write(&path, [0xff, 0xfe]).await.unwrap();
    let error = ReadFileTool
        .execute(json!({ "path": path }))
        .await
        .unwrap_err();
    assert!(error.contains("not valid UTF-8"));
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn preserves_trailing_whitespace_on_the_last_displayed_line() {
    let path = temp_path("trailing-whitespace.txt");
    fs::write(&path, "first\nlast  \t\n").await.unwrap();
    let result = ReadFileTool.execute(json!({ "path": path })).await.unwrap();
    assert!(result.ends_with("    2 | last  \t"), "{result:?}");
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn reports_a_poisoned_snapshot_store() {
    let path = temp_path("poisoned-snapshot.txt");
    fs::write(&path, "content\n").await.unwrap();
    let context = ToolExecutionContext::default();
    let snapshots = context.snapshots.clone();
    let _ = std::thread::spawn(move || {
        let _guard = snapshots.write().unwrap();
        panic!("poison snapshot store for test");
    })
    .join();

    let error = ReadFileTool
        .execute_output_live(json!({ "path": path }), None, context)
        .await
        .unwrap_err();
    assert!(error.contains("snapshot store lock is poisoned"), "{error}");
    let _ = fs::remove_file(path).await;
}

#[test]
fn schema_is_small_and_bounded() {
    let schema = ReadFileTool.definition().input_schema;
    assert_eq!(schema["required"], json!(["path"]));
    assert_eq!(schema["properties"]["max_lines"]["maximum"], 1000);
    assert_eq!(schema["additionalProperties"], false);
}

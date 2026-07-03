mod common;

use std::path::PathBuf;

use serde_json::json;
use tokio::fs;

use bone_core::tools::read_file::ReadFileTool;
use bone_core::tools::types::Tool;

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

    // Full read of a small file: content plus a size-awareness footer, no
    // paging prompt.
    assert_eq!(result, "line one\nline two\nline three\n\n[3 lines total]");
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

    // Ranged read to EOF: explicit range so the model knows it hit the end.
    assert_eq!(
        result,
        "gamma\ndelta\n\n[showing lines 3-4 of 4; end of file]"
    );
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

    // Partial view: tells the model the range and how to page.
    assert_eq!(
        result,
        "b\nc\n\n[showing lines 2-3 of 5; call again with start_line=4 to continue]"
    );
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

    // Empty range still reports total size so the model can recover.
    assert_eq!(result, "[no lines in range; file has 1 line]");
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn png_returns_image_output() {
    let dir = common::temp_dir("read-file-image");
    fs::create_dir_all(&dir).await.expect("setup dir");
    let path = dir.join("image.png");
    fs::write(&path, [137, 80, 78, 71]).await.expect("setup");
    let tool = ReadFileTool;

    let result = tool
        .execute_output(json!({ "path": path }))
        .await
        .expect("read image should succeed");

    assert_eq!(result.images.len(), 1);
    assert_eq!(result.images[0].media_type, "image/png");
    assert!(result.content.contains("image/png"));
    let _ = fs::remove_dir_all(&dir).await;
}

#[tokio::test]
async fn large_file_shows_paging_footer() {
    // A file over the default 500-line cap: the model must be told it is seeing
    // a partial view and how to continue.
    let body: String = (1..=600)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let path = temp_path("large.txt");
    fs::write(&path, body).await.expect("setup");
    let tool = ReadFileTool;

    let result = tool
        .execute(json!({ "path": path }))
        .await
        .expect("read should succeed");

    assert!(
        result.contains("[showing lines 1-500 of 600; call again with start_line=501 to continue]"),
        "missing paging footer: {result}"
    );
    // Last line of the window is present, line 501 is not.
    assert!(result.contains("line 500"));
    assert!(!result.contains("\nline 501\n"));
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn paging_continues_from_start_line() {
    let body: String = (1..=600)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let path = temp_path("large-page2.txt");
    fs::write(&path, body).await.expect("setup");
    let tool = ReadFileTool;

    let result = tool
        .execute(json!({ "path": path, "start_line": 501 }))
        .await
        .expect("read should succeed");

    // Reading the tail reaches EOF: explicit range, no paging prompt.
    assert!(result.contains("line 600"));
    assert!(result.contains("[showing lines 501-600 of 600; end of file]"));
    assert!(!result.contains("call again"));
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn long_single_line_is_capped() {
    // A single minified mega-line must not flood the context window.
    let mega = "x".repeat(5000);
    let path = temp_path("mega.txt");
    fs::write(&path, &mega).await.expect("setup");
    let tool = ReadFileTool;

    let result = tool
        .execute(json!({ "path": path }))
        .await
        .expect("read should succeed");

    assert!(result.contains("…[truncated]"), "missing truncation marker");
    // The kept body is bounded; the marker plus footer keep it well short of
    // the raw 5000-char line.
    assert!(result.len() < 5000);
    assert!(result.contains("[1 line total]"));
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn refuses_oversized_text_file_before_reading() {
    let path = temp_path("oversized.txt");
    let file = fs::File::create(&path).await.expect("setup");
    file.set_len(51 * 1024 * 1024).await.expect("set len");
    let tool = ReadFileTool;

    let result = tool.execute(json!({ "path": path })).await;

    assert!(result.is_err());
    assert!(result.unwrap_err().contains("too large to read directly"));
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn invalid_utf8_gets_instructive_error() {
    let path = temp_path("binary.bin");
    fs::write(&path, [0xff, 0xfe, 0xfd]).await.expect("setup");
    let tool = ReadFileTool;

    let result = tool.execute(json!({ "path": path })).await;

    assert!(result.is_err());
    assert!(result.unwrap_err().contains("not valid UTF-8"));
    let _ = fs::remove_file(&path).await;
}

mod common;

use std::path::PathBuf;

use bone_core::tools::edit_file::{EditFileTool, preview_edit_file};
use bone_core::tools::read_file::ReadFileTool;
use bone_core::tools::types::{Tool, ToolExecutionContext};
use bone_core::tools::write_atomic::write_atomic_if_unchanged;
use serde_json::json;
use tokio::fs;

fn temp_path(name: &str) -> PathBuf {
    common::temp_path(&format!("simple-edit-{name}"))
}

async fn setup(name: &str, content: &str) -> PathBuf {
    let path = temp_path(name);
    fs::write(&path, content).await.expect("setup");
    path
}

async fn read_into_context(path: &PathBuf, context: &ToolExecutionContext) {
    ReadFileTool
        .execute_output_live(json!({ "path": path }), None, context.clone())
        .await
        .expect("read");
}

async fn edit_live(
    path: &PathBuf,
    old: &str,
    new: &str,
    context: &ToolExecutionContext,
) -> Result<String, String> {
    EditFileTool
        .execute_output_live(
            json!({ "path": path, "old_text": old, "new_text": new }),
            None,
            context.clone(),
        )
        .await
        .map(|out| out.content)
}

#[tokio::test]
async fn replaces_exact_unique_text_after_read() {
    let path = setup("replace.txt", "alpha\nbeta\ngamma\n").await;
    let context = ToolExecutionContext::default();
    read_into_context(&path, &context).await;

    let result = edit_live(&path, "beta", "BETA", &context).await.unwrap();
    assert!(result.contains("Edited:"));
    assert!(result.contains("BETA"));
    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "alpha\nBETA\ngamma\n"
    );
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn deletion_and_contextual_insertion_use_the_same_contract() {
    let path = setup("delete-insert.txt", "one\ntwo\nthree\n").await;
    let context = ToolExecutionContext::default();
    read_into_context(&path, &context).await;
    edit_live(&path, "two\n", "", &context).await.unwrap();
    edit_live(&path, "one\nthree", "one\ntwo-and-a-half\nthree", &context)
        .await
        .unwrap();
    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "one\ntwo-and-a-half\nthree\n"
    );
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn can_insert_into_an_empty_file() {
    let path = setup("empty.txt", "").await;
    let context = ToolExecutionContext::default();
    read_into_context(&path, &context).await;
    edit_live(&path, "", "first line\n", &context)
        .await
        .unwrap();
    assert_eq!(fs::read_to_string(&path).await.unwrap(), "first line\n");
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn rejects_missing_and_ambiguous_old_text() {
    let path = setup("matches.txt", "same\nother\nsame\n").await;
    let context = ToolExecutionContext::default();
    read_into_context(&path, &context).await;

    let ambiguous = edit_live(&path, "same", "changed", &context)
        .await
        .unwrap_err();
    assert!(ambiguous.contains("more than once"), "{ambiguous}");
    let missing = edit_live(&path, "missing", "changed", &context)
        .await
        .unwrap_err();
    assert!(missing.contains("not found"), "{missing}");
    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "same\nother\nsame\n"
    );
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn requires_read_for_context_aware_calls() {
    let path = setup("unread.txt", "old\n").await;
    let error = edit_live(&path, "old", "new", &ToolExecutionContext::default())
        .await
        .unwrap_err();
    assert!(error.contains("read_file before editing"), "{error}");
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn rejects_text_outside_the_read_range() {
    let path = setup("range.txt", "one\ntwo\nthree\nfour\n").await;
    let context = ToolExecutionContext::default();
    ReadFileTool
        .execute_output_live(
            json!({ "path": path, "start_line": 2, "max_lines": 1 }),
            None,
            context.clone(),
        )
        .await
        .unwrap();
    let error = edit_live(&path, "three", "THREE", &context)
        .await
        .unwrap_err();
    assert!(error.contains("not shown"), "{error}");
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn tolerates_unrelated_live_drift_when_match_remains_unique() {
    let path = setup("drift.txt", "alpha\nbeta\ngamma\n").await;
    let context = ToolExecutionContext::default();
    read_into_context(&path, &context).await;
    fs::write(&path, "prefix\nalpha\nbeta\ngamma\n")
        .await
        .unwrap();

    edit_live(&path, "beta", "BETA", &context).await.unwrap();
    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "prefix\nalpha\nBETA\ngamma\n"
    );
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn stale_conflict_requests_a_reread() {
    let path = setup("conflict.txt", "alpha\nbeta\n").await;
    let context = ToolExecutionContext::default();
    read_into_context(&path, &context).await;
    fs::write(&path, "alpha\nBETA ELSEWHERE\n").await.unwrap();

    let error = edit_live(&path, "beta", "BETA", &context)
        .await
        .unwrap_err();
    assert!(error.contains("changed after it was read"), "{error}");
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn relative_and_absolute_paths_share_snapshot_identity() {
    let path = setup("path.txt", "old\n").await;
    let context = ToolExecutionContext::default();
    read_into_context(&path, &context).await;
    let mut relative = PathBuf::new();
    for _ in std::env::current_dir().unwrap().components().skip(1) {
        relative.push("..");
    }
    relative.push(path.strip_prefix("/").unwrap());
    edit_live(&relative, "old", "new", &context).await.unwrap();
    assert_eq!(fs::read_to_string(&path).await.unwrap(), "new\n");
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn partial_read_stays_partial_after_an_edit() {
    let path = setup("partial-after-edit.txt", "one\ntwo\nthree\nfour\n").await;
    let context = ToolExecutionContext::default();
    ReadFileTool
        .execute_output_live(
            json!({ "path": path, "start_line": 2, "max_lines": 1 }),
            None,
            context.clone(),
        )
        .await
        .unwrap();

    edit_live(&path, "two", "TWO", &context).await.unwrap();
    let error = edit_live(&path, "four", "FOUR", &context)
        .await
        .unwrap_err();
    assert!(error.contains("not shown"), "{error}");
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn conditional_atomic_write_rejects_changed_destination() {
    let path = setup("conditional-write.txt", "current\n").await;
    let error = write_atomic_if_unchanged(&path, "replacement\n", None, b"stale\n")
        .await
        .unwrap_err();
    assert!(
        error.contains("changed while the edit was being prepared"),
        "{error}"
    );
    assert_eq!(fs::read_to_string(&path).await.unwrap(), "current\n");
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn preview_uses_the_simple_schema_without_writing() {
    let path = setup("preview.txt", "old\n").await;
    let preview = preview_edit_file(
        "edit_file",
        json!({ "path": path, "old_text": "old", "new_text": "new" }),
    )
    .await
    .unwrap();
    assert!(preview.diff.contains("new"));
    assert_eq!(fs::read_to_string(&path).await.unwrap(), "old\n");
    let _ = fs::remove_file(path).await;
}

#[test]
fn schema_has_only_the_three_simple_fields() {
    let schema = EditFileTool.definition().input_schema;
    assert_eq!(schema["required"], json!(["path", "old_text", "new_text"]));
    assert!(schema["properties"].get("input").is_none());
    assert_eq!(schema["additionalProperties"], false);
}

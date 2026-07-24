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
async fn preserves_crlf_and_bom() {
    let path = temp_path("crlf-bom.txt");
    fs::write(&path, "\u{feff}alpha\r\nbeta\r\ngamma\r\n")
        .await
        .unwrap();
    let context = ToolExecutionContext::default();
    read_into_context(&path, &context).await;

    edit_live(&path, "beta", "BETA", &context).await.unwrap();
    assert_eq!(
        fs::read(&path).await.unwrap(),
        "\u{feff}alpha\r\nBETA\r\ngamma\r\n".as_bytes()
    );
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn preserves_mixed_line_endings_outside_the_edit() {
    let path = temp_path("mixed-endings.txt");
    fs::write(&path, "alpha\r\nbeta\ngamma\rdelta")
        .await
        .unwrap();
    let context = ToolExecutionContext::default();
    read_into_context(&path, &context).await;

    edit_live(&path, "beta", "BETA", &context).await.unwrap();
    assert_eq!(
        fs::read(&path).await.unwrap(),
        b"alpha\r\nBETA\ngamma\rdelta"
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
async fn rejects_external_changes_even_when_the_match_remains_unique() {
    let path = setup("drift.txt", "alpha\nbeta\ngamma\n").await;
    let context = ToolExecutionContext::default();
    read_into_context(&path, &context).await;
    fs::write(&path, "prefix\nalpha\nbeta\ngamma\n")
        .await
        .unwrap();

    let error = edit_live(&path, "beta", "BETA", &context)
        .await
        .unwrap_err();
    assert!(error.contains("changed after it was read"), "{error}");
    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "prefix\nalpha\nbeta\ngamma\n"
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
    let project_dir = path.parent().unwrap().to_path_buf();
    let relative = PathBuf::from(path.file_name().unwrap());
    let context = ToolExecutionContext::default().with_working_dir(project_dir);
    read_into_context(&relative, &context).await;
    edit_live(&path, "old", "new", &context).await.unwrap();
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
async fn preserves_other_visible_lines_after_an_edit() {
    let path = setup("two-edits.txt", "one\ntwo\nthree\nfour\nfive\n").await;
    let context = ToolExecutionContext::default();
    ReadFileTool
        .execute_output_live(
            json!({ "path": path, "start_line": 2, "max_lines": 3 }),
            None,
            context.clone(),
        )
        .await
        .unwrap();

    edit_live(&path, "two", "TWO", &context).await.unwrap();
    edit_live(&path, "four", "FOUR", &context).await.unwrap();
    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "one\nTWO\nthree\nFOUR\nfive\n"
    );
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn newline_replacement_preserves_visible_unchanged_suffix() {
    let path = setup("newline-suffix.txt", "abcDEFghi").await;
    let context = ToolExecutionContext::default();
    ReadFileTool
        .execute_output_live(json!({ "path": path }), None, context.clone())
        .await
        .unwrap();

    edit_live(&path, "DEF", "X\n", &context).await.unwrap();
    edit_live(&path, "ghi", "GHI", &context).await.unwrap();
    assert_eq!(fs::read_to_string(&path).await.unwrap(), "abcX\nGHI");
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn deletion_shifts_later_visible_lines() {
    let path = setup("delete-shift.txt", "one\ntwo\nthree\nfour\nfive\n").await;
    let context = ToolExecutionContext::default();
    ReadFileTool
        .execute_output_live(
            json!({ "path": path, "start_line": 2, "max_lines": 3 }),
            None,
            context.clone(),
        )
        .await
        .unwrap();

    edit_live(&path, "two\n", "", &context).await.unwrap();
    edit_live(&path, "four", "FOUR", &context).await.unwrap();
    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "one\nthree\nFOUR\nfive\n"
    );
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn insertion_shifts_later_visible_lines() {
    let path = setup("insert-shift.txt", "one\ntwo\nthree\nfour\nfive\n").await;
    let context = ToolExecutionContext::default();
    ReadFileTool
        .execute_output_live(
            json!({ "path": path, "start_line": 2, "max_lines": 3 }),
            None,
            context.clone(),
        )
        .await
        .unwrap();

    edit_live(&path, "two", "two\ninserted", &context)
        .await
        .unwrap();
    edit_live(&path, "four", "FOUR", &context).await.unwrap();
    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "one\ntwo\ninserted\nthree\nFOUR\nfive\n"
    );
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
async fn preview_uses_session_working_dir_without_writing() {
    let path = setup("preview.txt", "old\n").await;
    let project_dir = path.parent().unwrap();
    let relative = path.file_name().unwrap().to_string_lossy();
    let preview = preview_edit_file(
        "edit_file",
        json!({ "path": relative, "old_text": "old", "new_text": "new" }),
        Some(project_dir),
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

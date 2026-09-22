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

fn edits_args(path: &PathBuf, edits: &[(&str, &str)]) -> serde_json::Value {
    json!({
        "path": path,
        "edits": edits
            .iter()
            .map(|(old, new)| json!({ "old_text": old, "new_text": new }))
            .collect::<Vec<_>>(),
    })
}

async fn edit_live_edits(
    path: &PathBuf,
    edits: &[(&str, &str)],
    context: &ToolExecutionContext,
) -> Result<String, String> {
    EditFileTool
        .execute_output_live(edits_args(path, edits), None, context.clone())
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

#[tokio::test]
async fn applies_multiple_hunks_in_one_call() {
    let path = setup("multi-hunk.txt", "alpha\nbeta\ngamma\ndelta\n").await;
    let context = ToolExecutionContext::default();
    read_into_context(&path, &context).await;

    let result = edit_live_edits(&path, &[("alpha", "ALPHA"), ("gamma", "GAMMA")], &context)
        .await
        .unwrap();
    assert!(result.contains("Edited:"));
    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "ALPHA\nbeta\nGAMMA\ndelta\n"
    );
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn hunks_match_the_original_text_not_intermediate_results() {
    // "x" only exists once the first hunk has applied; it must not be
    // matchable by a later hunk in the same call.
    let path = setup("hunk-order.txt", "one\ntwo\n").await;
    let context = ToolExecutionContext::default();
    read_into_context(&path, &context).await;

    let error = edit_live_edits(&path, &[("one", "x"), ("x", "y")], &context)
        .await
        .unwrap_err();
    assert!(error.contains("not found"), "{error}");
    assert!(error.contains("hunk 2 of 2"), "{error}");
    assert!(error.contains("1 earlier hunks matched"), "{error}");
    assert!(error.contains("no changes written"), "{error}");
    assert_eq!(fs::read_to_string(&path).await.unwrap(), "one\ntwo\n");
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn rejects_duplicate_and_overlapping_hunks() {
    let path = setup("overlap.txt", "same\nother\nsame\n").await;
    let context = ToolExecutionContext::default();
    read_into_context(&path, &context).await;

    let duplicate = edit_live_edits(&path, &[("other", "O"), ("other", "X")], &context)
        .await
        .unwrap_err();
    assert!(duplicate.contains("same replacement twice"), "{duplicate}");
    assert!(duplicate.contains("hunks 1 and 2"), "{duplicate}");

    // "same\nother" spans the second hunk's match.
    let overlap = edit_live_edits(&path, &[("same\nother", "X"), ("other", "O")], &context)
        .await
        .unwrap_err();
    assert!(overlap.contains("hunks 1 and 2 overlap"), "{overlap}");
    assert!(overlap.contains("combine them"), "{overlap}");
    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "same\nother\nsame\n"
    );
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn rejects_mixed_forms_empty_edits_and_split_pairs() {
    let path = setup("mixed-form.txt", "old\n").await;
    let context = ToolExecutionContext::default();
    read_into_context(&path, &context).await;

    let mixed = EditFileTool
        .execute_output_live(
            json!({ "path": path, "old_text": "old", "new_text": "new", "edits": [] }),
            None,
            context.clone(),
        )
        .await
        .unwrap_err();
    assert!(mixed.contains("not both"), "{mixed}");

    let empty = edit_live_edits(&path, &[], &context).await.unwrap_err();
    assert!(empty.contains("must not be empty"), "{empty}");

    let split = EditFileTool
        .execute_output_live(
            json!({ "path": path, "old_text": "old" }),
            None,
            context.clone(),
        )
        .await
        .unwrap_err();
    assert!(split.contains("together"), "{split}");

    let missing = EditFileTool
        .execute_output_live(json!({ "path": path }), None, context.clone())
        .await
        .unwrap_err();
    assert!(missing.contains("old_text"), "{missing}");
    assert_eq!(fs::read_to_string(&path).await.unwrap(), "old\n");
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn multi_hunk_preserves_crlf_and_bom() {
    let path = temp_path("multi-crlf.txt");
    fs::write(&path, "\u{feff}alpha\r\nbeta\r\ngamma\r\n")
        .await
        .unwrap();
    let context = ToolExecutionContext::default();
    read_into_context(&path, &context).await;

    edit_live_edits(&path, &[("alpha", "ALPHA"), ("gamma", "GAMMA")], &context)
        .await
        .unwrap();
    assert_eq!(
        fs::read(&path).await.unwrap(),
        "\u{feff}ALPHA\r\nbeta\r\nGAMMA\r\n".as_bytes()
    );
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn rejects_when_any_hunk_falls_outside_the_read_range() {
    let path = setup("hunk-range.txt", "one\ntwo\nthree\nfour\n").await;
    let context = ToolExecutionContext::default();
    ReadFileTool
        .execute_output_live(
            json!({ "path": path, "start_line": 1, "max_lines": 2 }),
            None,
            context.clone(),
        )
        .await
        .unwrap();

    // Input order differs from file order; diagnostics must use input indexes.
    let error = edit_live_edits(
        &path,
        &[("two", "TWO"), ("four", "FOUR"), ("one", "ONE")],
        &context,
    )
    .await
    .unwrap_err();
    assert!(error.contains("not shown"), "{error}");
    assert!(error.contains("hunk 2 of 3"), "{error}");
    assert!(error.contains("read lines 4-4 with read_file"), "{error}");
    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "one\ntwo\nthree\nfour\n"
    );
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn follow_up_edit_succeeds_without_reread_after_multi_hunk() {
    let path = setup("follow-up.txt", "one\ntwo\nthree\nfour\n").await;
    let context = ToolExecutionContext::default();
    read_into_context(&path, &context).await;

    // Both targets of the second/third calls were produced by the first call
    // and have never been re-read.
    edit_live_edits(
        &path,
        &[("two", "TWO inserted"), ("four", "FOUR inserted")],
        &context,
    )
    .await
    .unwrap();
    edit_live(&path, "TWO inserted", "TWO edited", &context)
        .await
        .unwrap();
    edit_live(&path, "FOUR inserted", "FOUR edited", &context)
        .await
        .unwrap();
    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "one\nTWO edited\nthree\nFOUR edited\n"
    );
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn follow_up_edit_after_multiline_batch_uses_shifted_visibility() {
    let path = setup("multiline-batch-follow-up.txt", "one\ntwo\nthree\nfour\n").await;
    let context = ToolExecutionContext::default();
    read_into_context(&path, &context).await;

    edit_live_edits(
        &path,
        &[("one", "one\ninserted"), ("four", "FOUR")],
        &context,
    )
    .await
    .unwrap();
    edit_live(&path, "FOUR", "FOUR edited", &context)
        .await
        .unwrap();
    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "one\ninserted\ntwo\nthree\nFOUR edited\n"
    );
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn inline_deletion_preserves_visibility_of_later_lines() {
    let path = setup("inline-delete-follow-up.txt", "prefix foo\nmiddle\nlast\n").await;
    let context = ToolExecutionContext::default();
    read_into_context(&path, &context).await;

    edit_live(&path, "foo", "", &context).await.unwrap();
    edit_live(&path, "last", "LAST", &context).await.unwrap();
    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "prefix \nmiddle\nLAST\n"
    );
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn preview_and_execution_agree_on_batch_errors() {
    let path = setup("preview-errors.txt", "one\ntwo\nthree\n").await;
    let context = ToolExecutionContext::default();
    read_into_context(&path, &context).await;
    for edits in [
        vec![("three", "THREE"), ("missing", "X")],
        vec![("one", "ONE"), ("one", "X")],
        vec![("two", "TWO"), ("one\ntwo", "X")],
        vec![("one", "one")],
    ] {
        let preview_error = preview_edit_file("edit_file", edits_args(&path, &edits), None)
            .await
            .err()
            .expect("preview must fail");
        let error = edit_live_edits(&path, &edits, &context).await.unwrap_err();
        assert_eq!(preview_error, error);
        assert_eq!(
            fs::read_to_string(&path).await.unwrap(),
            "one\ntwo\nthree\n"
        );
    }
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn preview_matches_executed_diff_with_unicode_and_format_preservation() {
    let original = "\u{feff}alpha\r\ncafé\r\n東京\r\n";
    let path = setup("preview-unicode.txt", original).await;
    let context = ToolExecutionContext::default();
    read_into_context(&path, &context).await;
    let edits = &[("東京", "大阪\n京都"), ("café", "thé")];
    let preview = preview_edit_file("edit_file", edits_args(&path, edits), None)
        .await
        .unwrap();
    assert_eq!(fs::read_to_string(&path).await.unwrap(), original);
    let output = edit_live_edits(&path, edits, &context).await.unwrap();
    assert_eq!(output.split_once('\n').unwrap().1, preview.diff.trim_end());
    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "\u{feff}alpha\r\nthé\r\n大阪\r\n京都\r\n"
    );
    edit_live(&path, "京都", "Kyoto", &context).await.unwrap();
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn partial_mixed_forms_report_mixing_not_empty_edits() {
    let path = setup("partial-mixed.txt", "old\n").await;
    for field in ["old_text", "new_text"] {
        let mut args = edits_args(&path, &[("old", "new")]);
        args[field] = json!("old");
        let error = EditFileTool.execute(args).await.unwrap_err();
        assert_eq!(error, "provide either old_text/new_text or edits, not both");
    }
    assert_eq!(fs::read_to_string(&path).await.unwrap(), "old\n");
    let _ = fs::remove_file(path).await;
}

#[test]
fn schema_requires_path_and_advertises_both_forms() {
    let schema = EditFileTool.definition().input_schema;
    assert_eq!(schema["required"], json!(["path"]));
    assert!(schema["properties"].get("input").is_none());
    assert_eq!(schema["additionalProperties"], false);
    let edits = &schema["properties"]["edits"];
    assert_eq!(edits["type"], "array");
    assert_eq!(edits["items"]["required"], json!(["old_text", "new_text"]));
    assert_eq!(edits["items"]["additionalProperties"], false);
    assert!(EditFileTool.definition().description.contains("edits"));
}

mod common;

use std::path::PathBuf;

use serde_json::json;
use tokio::fs;

use bone_core::tools::edit_file::{EditFileTool, preview_edit_file};
use bone_core::tools::snapshot;
use bone_core::tools::types::{Tool, ToolExecutionContext};

fn temp_path(name: &str) -> PathBuf {
    common::temp_path(&format!("edit-file-{name}"))
}

/// Write a temp file and return its path.
async fn setup(name: &str, content: &str) -> PathBuf {
    let path = temp_path(name);
    fs::write(&path, content).await.expect("setup");
    path
}

/// Build a single-section hashline patch.
fn patch(path: &str, ops: &str) -> String {
    // Tag is irrelevant in the degraded `execute()` path (no snapshot store),
    // but the parser still requires a 4-hex header.
    format!("[{path}#A1B2]\n{ops}")
}

// ── SWAP ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn swap_single_line() {
    let path = setup("swap-single.txt", "alpha\nbeta\ngamma\n").await;
    let tool = EditFileTool;

    tool.execute(json!({ "input": patch(&path.to_string_lossy(), "SWAP 2.=2:\n+BETA") }))
        .await
        .expect("success");

    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "alpha\nBETA\ngamma\n"
    );
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn swap_range() {
    let path = setup("swap-range.txt", "one\ntwo\nthree\nfour\n").await;
    let tool = EditFileTool;

    tool.execute(json!({ "input": patch(&path.to_string_lossy(), "SWAP 2.=3:\n+TWO\n+THREE") }))
        .await
        .expect("success");

    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "one\nTWO\nTHREE\nfour\n"
    );
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn swap_expands_one_line_to_many() {
    let path = setup("swap-expand.txt", "old\n").await;
    let tool = EditFileTool;

    tool.execute(json!({ "input": patch(&path.to_string_lossy(), "SWAP 1.=1:\n+a\n+b\n+c") }))
        .await
        .expect("success");

    assert_eq!(fs::read_to_string(&path).await.unwrap(), "a\nb\nc\n");
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn swap_bare_line_number_shorthand() {
    // `SWAP N:` is tolerated as `SWAP N.=N:`.
    let path = setup("swap-shorthand.txt", "alpha\nbeta\n").await;
    let tool = EditFileTool;

    tool.execute(json!({ "input": patch(&path.to_string_lossy(), "SWAP 1:\n+ALPHA") }))
        .await
        .expect("success");

    assert_eq!(fs::read_to_string(&path).await.unwrap(), "ALPHA\nbeta\n");
    let _ = fs::remove_file(&path).await;
}

// ── DEL ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn del_single_line() {
    let path = setup("del-single.txt", "one\ntwo\nthree\n").await;
    let tool = EditFileTool;

    tool.execute(json!({ "input": patch(&path.to_string_lossy(), "DEL 2") }))
        .await
        .expect("success");

    assert_eq!(fs::read_to_string(&path).await.unwrap(), "one\nthree\n");
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn del_range() {
    let path = setup("del-range.txt", "a\nb\nc\nd\n").await;
    let tool = EditFileTool;

    tool.execute(json!({ "input": patch(&path.to_string_lossy(), "DEL 2.=3") }))
        .await
        .expect("success");

    assert_eq!(fs::read_to_string(&path).await.unwrap(), "a\nd\n");
    let _ = fs::remove_file(&path).await;
}

// ── INS ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn ins_pre() {
    let path = setup("ins-pre.txt", "one\nthree\n").await;
    let tool = EditFileTool;

    tool.execute(json!({ "input": patch(&path.to_string_lossy(), "INS.PRE 2:\n+two") }))
        .await
        .expect("success");

    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "one\ntwo\nthree\n"
    );
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn ins_post() {
    let path = setup("ins-post.txt", "one\ntwo\n").await;
    let tool = EditFileTool;

    tool.execute(json!({ "input": patch(&path.to_string_lossy(), "INS.POST 1:\n+one.five") }))
        .await
        .expect("success");

    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "one\none.five\ntwo\n"
    );
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn ins_head() {
    let path = setup("ins-head.txt", "two\nthree\n").await;
    let tool = EditFileTool;

    tool.execute(json!({ "input": patch(&path.to_string_lossy(), "INS.HEAD:\n+one") }))
        .await
        .expect("success");

    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "one\ntwo\nthree\n"
    );
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn ins_tail() {
    let path = setup("ins-tail.txt", "one\ntwo\n").await;
    let tool = EditFileTool;

    tool.execute(json!({ "input": patch(&path.to_string_lossy(), "INS.TAIL:\n+three") }))
        .await
        .expect("success");

    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "one\ntwo\nthree\n"
    );
    let _ = fs::remove_file(&path).await;
}

// ── Multi-hunk (original-line semantics) ────────────────────────────────────

#[tokio::test]
async fn multi_hunk_original_line_semantics() {
    // Line numbers refer to the original snapshot, not shifted by earlier hunks.
    let path = setup("multi-hunk.txt", "a\nb\nc\nd\n").await;
    let tool = EditFileTool;

    tool.execute(json!({
        "input": patch(
            &path.to_string_lossy(),
            "DEL 2\nSWAP 4.=4:\n+D\nINS.PRE 1:\n+prefix"
        )
    }))
    .await
    .expect("success");

    // DEL 2 removes "b", SWAP 4 replaces "d" (original line 4), INS.PRE 1 inserts before "a".
    assert_eq!(
        fs::read_to_string(&path).await.unwrap(),
        "prefix\na\nc\nD\n"
    );
    let _ = fs::remove_file(&path).await;
}

// ── File lifecycle: MV / REM ────────────────────────────────────────────────

#[tokio::test]
async fn duplicate_sections_are_rejected_without_writing() {
    let path = setup("duplicate-sections.txt", "one\ntwo\n").await;
    let path_text = path.to_string_lossy();
    let input = format!("[{path_text}#A1B2]\nSWAP 1:\n+ONE\n[{path_text}#A1B2]\nSWAP 2:\n+TWO");
    let error = EditFileTool
        .execute(json!({ "input": input }))
        .await
        .expect_err("duplicate paths must fail preflight");
    assert!(error.contains("duplicate section"), "{error}");
    assert_eq!(fs::read_to_string(&path).await.unwrap(), "one\ntwo\n");
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn unknown_snapshot_tag_is_rejected_without_writing() {
    let path = setup("unknown-tag.txt", "one\ntwo\n").await;
    let error = EditFileTool
        .execute_output_live(
            json!({
                "input": patch(&path.to_string_lossy(), "SWAP 1:\n+ONE")
            }),
            None,
            ToolExecutionContext::default(),
        )
        .await
        .expect_err("unknown tags must require a re-read");
    assert!(error.contains("was not found; re-read"), "{error}");
    assert_eq!(fs::read_to_string(&path).await.unwrap(), "one\ntwo\n");
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn exact_live_tag_recovers_when_snapshot_store_was_restarted() {
    // A resumed conversation can contain a read_file result whose in-memory
    // snapshot was lost when the daemon restarted. An exact live-content tag
    // is still sufficient to apply the anchored edit safely.
    let path = setup("restarted-snapshot.txt", "one\ntwo\n").await;
    let tag = snapshot::compute_tag("one\ntwo\n");
    let input = format!("[{}#{tag}]\nSWAP 2.=2:\n+TWO", path.to_string_lossy());

    let result = EditFileTool
        .execute_output_live(
            json!({ "input": input }),
            None,
            ToolExecutionContext::default(),
        )
        .await
        .expect("exact live tag should recover");

    assert!(result.content.contains("edited"));
    assert_eq!(fs::read_to_string(&path).await.unwrap(), "one\nTWO\n");
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn mv_renames_file() {
    let path = setup("mv-src.txt", "content\n").await;
    let dest = temp_path("mv-dst.txt");
    let _ = fs::remove_file(&dest).await; // clean slate
    let tool = EditFileTool;

    let result = tool
        .execute(json!({
            "input": format!("[{}#A1B2]\nMV {}", path.to_string_lossy(), dest.to_string_lossy())
        }))
        .await
        .expect("success");

    assert!(result.contains("renamed"));
    assert!(!path.exists());
    assert_eq!(fs::read_to_string(&dest).await.unwrap(), "content\n");
    let _ = fs::remove_file(&dest).await;
}

#[tokio::test]
async fn rem_deletes_file() {
    let path = setup("rem-src.txt", "content\n").await;
    let tool = EditFileTool;

    let result = tool
        .execute(json!({ "input": patch(&path.to_string_lossy(), "REM") }))
        .await
        .expect("success");

    assert!(result.contains("deleted"));
    assert!(!path.exists());
}

#[tokio::test]
async fn mv_mixed_with_content_op_rejected() {
    let path = setup("mv-mixed.txt", "content\n").await;
    let dest = temp_path("mv-mixed-dst.txt");
    let _ = fs::remove_file(&dest).await;
    let tool = EditFileTool;

    let result = tool
        .execute(json!({
            "input": format!(
                "[{}#A1B2]\nSWAP 1.=1:\n+changed\nMV {}",
                path.to_string_lossy(),
                dest.to_string_lossy()
            )
        }))
        .await;

    assert!(result.is_err());
    assert!(result.unwrap_err().contains("must be the only op"));
    // File unchanged.
    assert_eq!(fs::read_to_string(&path).await.unwrap(), "content\n");
    assert!(!dest.exists());
    let _ = fs::remove_file(&path).await;
    let _ = fs::remove_file(&dest).await;
}

// ── Multi-file ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn multi_file_success() {
    let p1 = setup("multi-1.txt", "one\n").await;
    let p2 = setup("multi-2.txt", "two\n").await;
    let tool = EditFileTool;

    let result = tool
        .execute(json!({
            "input": format!(
                "[{}#A1B2]\nSWAP 1.=1:\n+ONE\n[{}#A1B2]\nSWAP 1.=1:\n+TWO",
                p1.to_string_lossy(),
                p2.to_string_lossy()
            )
        }))
        .await
        .expect("success");

    assert!(result.contains("edited"));
    assert_eq!(fs::read_to_string(&p1).await.unwrap(), "ONE\n");
    assert_eq!(fs::read_to_string(&p2).await.unwrap(), "TWO\n");
    let _ = fs::remove_file(&p1).await;
    let _ = fs::remove_file(&p2).await;
}

#[tokio::test]
async fn prefailure_leaves_all_files_unchanged() {
    // Second section fails during preflight (out of bounds). All sections are
    // validated before any write, so neither file is touched.
    let p1 = setup("partial-1.txt", "one\n").await;
    let p2 = setup("partial-2.txt", "two\n").await;
    let tool = EditFileTool;

    let result = tool
        .execute(json!({
            "input": format!(
                "[{}#A1B2]\nSWAP 1.=1:\n+ONE\n[{}#A1B2]\nSWAP 99.=99:\n+BAD",
                p1.to_string_lossy(),
                p2.to_string_lossy()
            )
        }))
        .await;

    assert!(result.is_err());
    assert!(!result.unwrap_err().contains("wrote:"));
    // Neither file written.
    assert_eq!(fs::read_to_string(&p1).await.unwrap(), "one\n");
    assert_eq!(fs::read_to_string(&p2).await.unwrap(), "two\n");
    let _ = fs::remove_file(&p1).await;
    let _ = fs::remove_file(&p2).await;
}

#[tokio::test]
async fn mid_batch_write_failure_reports_written() {
    // First section writes; second section's MV fails at execution time
    // (destination parent doesn't exist). No rollback.
    let p1 = setup("midbatch-1.txt", "one\n").await;
    let p2 = setup("midbatch-2.txt", "two\n").await;
    let bad_dest = temp_path("nonexistent_dir/dst.txt");
    let tool = EditFileTool;

    let result = tool
        .execute(json!({
            "input": format!(
                "[{}#A1B2]\nSWAP 1.=1:\n+ONE\n[{}#A1B2]\nMV {}",
                p1.to_string_lossy(),
                p2.to_string_lossy(),
                bad_dest.to_string_lossy()
            )
        }))
        .await;

    assert!(result.is_err());
    let err = result.unwrap_err();
    // First file was written before the failure.
    assert!(err.contains("wrote:"));
    assert!(err.contains("ONE") || fs::read_to_string(&p1).await.unwrap() == "ONE\n");
    // Second file unchanged (MV failed).
    assert_eq!(fs::read_to_string(&p2).await.unwrap(), "two\n");
    assert!(!bad_dest.exists());
    let _ = fs::remove_file(&p1).await;
    let _ = fs::remove_file(&p2).await;
}

// ── Error handling ──────────────────────────────────────────────────────────

#[tokio::test]
async fn empty_input_rejected() {
    let path = setup("empty-input.txt", "content\n").await;
    let tool = EditFileTool;

    let result = tool.execute(json!({ "input": "" })).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("empty"));
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn missing_input_field_rejected() {
    let tool = EditFileTool;
    let result = tool.execute(json!({ "path": "/tmp/nonexistent" })).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("input"));
}

#[tokio::test]
async fn out_of_bounds_swap_rejected() {
    let path = setup("oob.txt", "one\ntwo\n").await;
    let tool = EditFileTool;

    let result = tool
        .execute(json!({ "input": patch(&path.to_string_lossy(), "SWAP 99.=99:\n+x") }))
        .await;

    assert!(result.is_err());
    assert!(result.unwrap_err().contains("out of bounds"));
    assert_eq!(fs::read_to_string(&path).await.unwrap(), "one\ntwo\n");
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn overlapping_ranges_rejected() {
    let path = setup("overlap.txt", "a\nb\nc\nd\n").await;
    let tool = EditFileTool;

    let result = tool
        .execute(json!({
            "input": patch(
                &path.to_string_lossy(),
                "SWAP 1.=3:\n+x\n+y\n+z\nDEL 2"
            )
        }))
        .await;

    assert!(result.is_err());
    assert!(result.unwrap_err().contains("overlap"));
    assert_eq!(fs::read_to_string(&path).await.unwrap(), "a\nb\nc\nd\n");
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn nonexistent_file_rejected() {
    let path = temp_path("does-not-exist.txt");
    let _ = fs::remove_file(&path).await;
    let tool = EditFileTool;

    let result = tool
        .execute(json!({ "input": patch(&path.to_string_lossy(), "SWAP 1.=1:\n+x") }))
        .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn directory_rejected() {
    let dir = common::temp_dir("edit-file-dir");
    fs::create_dir_all(&dir).await.expect("mkdir");
    let tool = EditFileTool;

    let result = tool
        .execute(json!({ "input": patch(&dir.to_string_lossy(), "SWAP 1.=1:\n+x") }))
        .await;

    assert!(result.is_err());
    assert!(result.unwrap_err().contains("not a regular file"));
    let _ = fs::remove_dir_all(&dir).await;
}

#[tokio::test]
async fn empty_body_on_swap_rejected() {
    let path = setup("empty-body.txt", "one\ntwo\n").await;
    let tool = EditFileTool;

    let result = tool
        .execute(json!({ "input": patch(&path.to_string_lossy(), "SWAP 1.=1:") }))
        .await;

    assert!(result.is_err());
    assert!(result.unwrap_err().contains("empty body"));
    assert_eq!(fs::read_to_string(&path).await.unwrap(), "one\ntwo\n");
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn noop_swap_rejected() {
    // SWAP that produces the same content → no-op guard.
    let path = setup("noop.txt", "same\n").await;
    let tool = EditFileTool;

    let result = tool
        .execute(json!({ "input": patch(&path.to_string_lossy(), "SWAP 1.=1:\n+same") }))
        .await;

    assert!(result.is_err());
    assert!(result.unwrap_err().contains("no change"));
    assert_eq!(fs::read_to_string(&path).await.unwrap(), "same\n");
    let _ = fs::remove_file(&path).await;
}

// ── Normalization ───────────────────────────────────────────────────────────

#[tokio::test]
async fn crlf_normalized_on_edit() {
    let path = setup("crlf.txt", "alpha\r\nbeta\r\n").await;
    let tool = EditFileTool;

    tool.execute(json!({ "input": patch(&path.to_string_lossy(), "SWAP 2.=2:\n+BETA") }))
        .await
        .expect("success");

    // CRLF → LF normalization: output has no \r.
    assert_eq!(fs::read_to_string(&path).await.unwrap(), "alpha\nBETA\n");
    let _ = fs::remove_file(&path).await;
}

#[tokio::test]
async fn no_trailing_newline_preserved() {
    let path = setup("no-nl.txt", "a\nb\nc").await;
    let tool = EditFileTool;

    tool.execute(json!({ "input": patch(&path.to_string_lossy(), "SWAP 2.=2:\n+B") }))
        .await
        .expect("success");

    assert_eq!(fs::read_to_string(&path).await.unwrap(), "a\nB\nc");
    let _ = fs::remove_file(&path).await;
}

// ── Schema ──────────────────────────────────────────────────────────────────

#[test]
fn schema_advertises_only_input_field() {
    let schema = EditFileTool.definition().input_schema;
    let props = &schema["properties"];
    assert!(props.get("input").is_some());
    assert!(props.get("path").is_none());
    assert!(props.get("search").is_none());
    assert!(props.get("replace").is_none());
    assert!(props.get("edits").is_none());
    assert!(props.get("mode").is_none());
    assert!(props.get("content").is_none());
    assert!(props.get("expected_hash").is_none());
    assert_eq!(schema["required"], json!(["input"]));
}

// ── Preview ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn preview_shows_diff_for_swap() {
    let path = setup("preview.txt", "old\n").await;
    let args = json!({ "input": patch(&path.to_string_lossy(), "SWAP 1.=1:\n+new") });

    let preview = preview_edit_file("edit_file", args).await.expect("preview");
    assert!(preview.diff.contains("old"));
    assert!(preview.diff.contains("new"));
    assert!(!preview.before_hash.is_empty());

    // Preview does not write.
    assert_eq!(fs::read_to_string(&path).await.unwrap(), "old\n");
    let _ = fs::remove_file(&path).await;
}

// ── Permissions ─────────────────────────────────────────────────────────────

#[cfg(unix)]
#[tokio::test]
async fn preserves_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let path = setup("perms.sh", "old\n").await;
    fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .await
        .expect("chmod");
    let tool = EditFileTool;

    tool.execute(json!({ "input": patch(&path.to_string_lossy(), "SWAP 1.=1:\n+new") }))
        .await
        .expect("success");

    let mode = fs::metadata(&path).await.unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o755);
    let _ = fs::remove_file(&path).await;
}

// ── Return format ───────────────────────────────────────────────────────────

#[tokio::test]
async fn success_message_includes_new_tag() {
    let path = setup("tag.txt", "old\n").await;
    let tool = EditFileTool;

    let result = tool
        .execute(json!({ "input": patch(&path.to_string_lossy(), "SWAP 1.=1:\n+new") }))
        .await
        .expect("success");

    // Result contains edited `<path>` and a new [path#TAG] header.
    assert!(result.contains("edited"));
    assert!(result.contains("#"));
    assert!(result.contains("new"));
    let _ = fs::remove_file(&path).await;
}

mod common;

use std::path::PathBuf;

use bone_core::tools::edit_file::{EditFileTool, line_hash, render_line};
use bone_core::tools::read_file::ReadFileTool;
use bone_core::tools::types::{Tool, ToolExecutionContext};
use serde_json::{Value, json};
use tokio::fs;

async fn setup(name: &str, content: &str) -> PathBuf {
    let path = common::temp_path(&format!("edit_file-{name}"));
    fs::write(&path, content).await.expect("setup");
    path
}

fn context() -> ToolExecutionContext {
    let context = ToolExecutionContext::default();
    context.snapshots.write().unwrap().set_hashline(true);
    context
}

async fn read(path: &PathBuf, context: &ToolExecutionContext) -> String {
    read_args(json!({ "path": path }), context).await
}

async fn read_args(args: Value, context: &ToolExecutionContext) -> String {
    ReadFileTool
        .execute_output_live(args, None, context.clone())
        .await
        .expect("read")
        .content
}

fn a(line: usize, content: &str) -> String {
    format!("{line}#{}", line_hash(content))
}

async fn edit(
    path: &PathBuf,
    edits: Value,
    context: &ToolExecutionContext,
) -> Result<String, String> {
    EditFileTool
        .execute_output_live(
            json!({ "path": path, "edits": edits }),
            None,
            context.clone(),
        )
        .await
        .map(|out| out.content)
}

async fn contents(path: &PathBuf) -> String {
    fs::read_to_string(path).await.unwrap()
}

const ABC: &str = "alpha\nbeta\ngamma\ndelta\nepsilon\n";

#[tokio::test]
async fn read_file_renders_anchors_in_edit_file_mode() {
    let path = setup("render.txt", ABC).await;
    let out = read(&path, &context()).await;
    assert!(out.contains(&render_line(2, "beta")), "{out}");
    assert!(out.contains(&format!("{}|beta", a(2, "beta"))), "{out}");
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn plain_read_is_unchanged_without_edit_file() {
    let path = setup("plain.txt", ABC).await;
    let out = read_args(json!({ "path": path }), &ToolExecutionContext::default()).await;
    assert!(!out.contains(&render_line(2, "beta")), "{out}");
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn replaces_single_line() {
    let path = setup("single.txt", ABC).await;
    let ctx = context();
    read(&path, &ctx).await;
    let out = edit(&path, json!([{ "at": a(2, "beta"), "text": "BETA" }]), &ctx)
        .await
        .unwrap();
    assert!(out.contains("Edited:"), "{out}");
    assert!(out.contains(&render_line(2, "BETA")), "{out}");
    assert_eq!(
        contents(&path).await,
        "alpha\nBETA\ngamma\ndelta\nepsilon\n"
    );
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn single_edit_at_top_level_is_accepted() {
    let path = setup("toplevel.txt", ABC).await;
    let ctx = context();
    read(&path, &ctx).await;
    EditFileTool
        .execute_output_live(
            json!({ "path": path, "at": a(1, "alpha"), "text": "ALPHA" }),
            None,
            ctx.clone(),
        )
        .await
        .unwrap();
    assert!(contents(&path).await.starts_with("ALPHA\nbeta\n"));
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn replaces_range_with_more_lines() {
    let path = setup("range.txt", ABC).await;
    let ctx = context();
    read(&path, &ctx).await;
    edit(
        &path,
        json!([{ "at": a(2, "beta"), "end": a(4, "delta"), "text": "x\ny\nz\nw" }]),
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(contents(&path).await, "alpha\nx\ny\nz\nw\nepsilon\n");
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn empty_text_deletes_range() {
    let path = setup("delete.txt", ABC).await;
    let ctx = context();
    read(&path, &ctx).await;
    edit(
        &path,
        json!([{ "at": a(2, "beta"), "end": a(3, "gamma"), "text": "" }]),
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(contents(&path).await, "alpha\ndelta\nepsilon\n");
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn inserts_after_before_and_at_start() {
    let path = setup("insert.txt", ABC).await;
    let ctx = context();
    read(&path, &ctx).await;
    edit(
        &path,
        json!([
            { "after": "0", "text": "head" },
            { "after": a(2, "beta"), "text": "after-beta" },
            { "before": a(5, "epsilon"), "text": "before-eps1\nbefore-eps2" },
        ]),
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(
        contents(&path).await,
        "head\nalpha\nbeta\nafter-beta\ngamma\ndelta\nbefore-eps1\nbefore-eps2\nepsilon\n"
    );
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn multi_edit_applies_against_one_snapshot() {
    let path = setup("multi.txt", ABC).await;
    let ctx = context();
    read(&path, &ctx).await;
    edit(
        &path,
        json!([
            { "at": a(5, "epsilon"), "text": "EPS" },
            { "at": a(1, "alpha"), "text": "A1\nA2" },
            { "at": a(3, "gamma"), "text": "" },
        ]),
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(contents(&path).await, "A1\nA2\nbeta\ndelta\nEPS\n");
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn overlapping_edits_are_rejected_atomically() {
    let path = setup("overlap.txt", ABC).await;
    let ctx = context();
    read(&path, &ctx).await;
    let err = edit(
        &path,
        json!([
            { "at": a(2, "beta"), "end": a(4, "delta"), "text": "X" },
            { "at": a(3, "gamma"), "text": "Y" },
        ]),
        &ctx,
    )
    .await
    .unwrap_err();
    assert!(err.contains("no changes written"), "{err}");
    assert_eq!(contents(&path).await, ABC);
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn chained_edits_reuse_old_anchors_without_reread() {
    let path = setup("chain.txt", ABC).await;
    let ctx = context();
    read(&path, &ctx).await;
    edit(&path, json!([{ "after": "0", "text": "n1\nn2" }]), &ctx)
        .await
        .unwrap();
    // `delta` moved from line 4 to 6, but the stale anchor still resolves.
    edit(
        &path,
        json!([{ "at": a(4, "delta"), "text": "DELTA" }]),
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(
        contents(&path).await,
        "n1\nn2\nalpha\nbeta\ngamma\nDELTA\nepsilon\n"
    );
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn external_insertion_is_remapped() {
    let path = setup("external.txt", ABC).await;
    let ctx = context();
    read(&path, &ctx).await;
    fs::write(&path, format!("// header\n{ABC}")).await.unwrap();
    edit(
        &path,
        json!([{ "at": a(3, "gamma"), "text": "GAMMA" }]),
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(
        contents(&path).await,
        "// header\nalpha\nbeta\nGAMMA\ndelta\nepsilon\n"
    );
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn changed_target_is_rejected_with_fresh_anchors_then_retry_works() {
    let path = setup("changed.txt", ABC).await;
    let ctx = context();
    read(&path, &ctx).await;
    fs::write(&path, "alpha\nbeta\nGAMMA!\ndelta\nepsilon\n")
        .await
        .unwrap();
    let err = edit(&path, json!([{ "at": a(3, "gamma"), "text": "x" }]), &ctx)
        .await
        .unwrap_err();
    assert!(err.contains("no changes written"), "{err}");
    let fresh = render_line(3, "GAMMA!");
    assert!(err.contains(&fresh), "{err}");
    assert_eq!(
        contents(&path).await,
        "alpha\nbeta\nGAMMA!\ndelta\nepsilon\n"
    );
    // Retry with the anchor from the error message, no re-read.
    edit(&path, json!([{ "at": a(3, "GAMMA!"), "text": "x" }]), &ctx)
        .await
        .unwrap();
    assert_eq!(contents(&path).await, "alpha\nbeta\nx\ndelta\nepsilon\n");
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn wrong_hash_is_rejected() {
    let path = setup("wrong.txt", ABC).await;
    let ctx = context();
    read(&path, &ctx).await;
    let err = edit(
        &path,
        json!([{ "at": a(2, "not beta"), "text": "x" }]),
        &ctx,
    )
    .await
    .unwrap_err();
    assert!(err.contains(&render_line(2, "beta")), "{err}");
    assert_eq!(contents(&path).await, ABC);
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn range_over_unseen_lines_is_rejected() {
    let body: String = (1..=300).map(|n| format!("line {n}\n")).collect();
    let path = setup("unseen.txt", &body).await;
    let ctx = context();
    read_args(json!({ "path": path, "max_lines": 10 }), &ctx).await;
    let err = edit(
        &path,
        json!([{ "at": a(2, "line 2"), "end": a(200, "line 200"), "text": "x" }]),
        &ctx,
    )
    .await
    .unwrap_err();
    assert!(err.contains("no changes written"), "{err}");
    assert_eq!(contents(&path).await, body);
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn preserves_crlf_and_bom() {
    let path = setup("crlf.txt", "\u{feff}alpha\r\nbeta\r\ngamma\r\n").await;
    let ctx = context();
    read(&path, &ctx).await;
    edit(
        &path,
        json!([{ "at": a(2, "beta"), "text": "b1\nb2" }]),
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(
        contents(&path).await,
        "\u{feff}alpha\r\nb1\r\nb2\r\ngamma\r\n"
    );
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn preserves_missing_trailing_newline() {
    let path = setup("notrail.txt", "alpha\nbeta").await;
    let ctx = context();
    read(&path, &ctx).await;
    edit(&path, json!([{ "at": a(2, "beta"), "text": "BETA" }]), &ctx)
        .await
        .unwrap();
    assert_eq!(contents(&path).await, "alpha\nBETA");
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn handles_unicode_lines() {
    let path = setup("unicode.txt", "héllo\n日本語\n🦀 crab\n").await;
    let ctx = context();
    read(&path, &ctx).await;
    edit(
        &path,
        json!([{ "at": a(2, "日本語"), "text": "中文" }]),
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(contents(&path).await, "héllo\n中文\n🦀 crab\n");
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn copied_anchor_prefixes_are_tolerated() {
    let path = setup("copied.txt", ABC).await;
    let ctx = context();
    read(&path, &ctx).await;
    let text = format!("{}\n{}", render_line(2, "B"), render_line(3, "G"));
    edit(
        &path,
        json!([{ "at": render_line(2, "beta"), "end": a(3, "gamma"), "text": text }]),
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(contents(&path).await, "alpha\nB\nG\ndelta\nepsilon\n");
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn no_op_edit_fails() {
    let path = setup("noop.txt", ABC).await;
    let ctx = context();
    read(&path, &ctx).await;
    let err = edit(&path, json!([{ "at": a(2, "beta"), "text": "beta" }]), &ctx)
        .await
        .unwrap_err();
    assert!(!err.is_empty());
    assert_eq!(contents(&path).await, ABC);
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn missing_path_fails() {
    let path = common::temp_path("edit_file-missing.txt");
    let err = edit(&path, json!([{ "at": "1#aa", "text": "x" }]), &context())
        .await
        .unwrap_err();
    assert!(!err.is_empty());
}

#[tokio::test]
async fn invalid_edit_shapes_are_rejected() {
    let path = setup("shape.txt", ABC).await;
    let ctx = context();
    read(&path, &ctx).await;
    for bad in [
        json!([{ "at": a(1, "alpha"), "after": a(2, "beta"), "text": "x" }]),
        json!([{ "after": a(1, "alpha"), "end": a(2, "beta"), "text": "x" }]),
        json!([{ "at": a(3, "gamma"), "end": a(1, "alpha"), "text": "x" }]),
        json!([{ "after": a(1, "alpha"), "text": "" }]),
        json!([{ "text": "x" }]),
    ] {
        assert!(edit(&path, bad.clone(), &ctx).await.is_err(), "{bad}");
    }
    assert_eq!(contents(&path).await, ABC);
    let _ = fs::remove_file(path).await;
}

const BLANKS: &str = "a\n\nb\nc\n\nd\n";

#[tokio::test]
async fn stale_low_entropy_anchor_after_own_edit_is_ambiguous() {
    let path = setup("ambiguous.txt", BLANKS).await;
    let ctx = context();
    read(&path, &ctx).await;
    edit(
        &path,
        json!([{ "after": a(1, "a"), "text": "x\ny\nz" }]),
        &ctx,
    )
    .await
    .unwrap();
    let after_insert = contents(&path).await;
    assert_eq!(after_insert, "a\nx\ny\nz\n\nb\nc\n\nd\n");
    // `5#..` from the first read meant the blank before `d` (now line 8), but
    // live line 5 is also blank. Guessing either would risk corruption.
    let err = edit(&path, json!([{ "at": a(5, ""), "text": "BLANK" }]), &ctx)
        .await
        .unwrap_err();
    assert!(err.contains("ambiguous"), "{err}");
    assert!(err.contains(&render_line(5, "")), "{err}");
    assert!(err.contains(&render_line(8, "")), "{err}");
    assert_eq!(contents(&path).await, after_insert);
    let _ = fs::remove_file(path).await;
}

#[tokio::test]
async fn stale_unique_anchor_after_own_edit_follows_the_line() {
    let path = setup("follow.txt", BLANKS).await;
    let ctx = context();
    read(&path, &ctx).await;
    edit(
        &path,
        json!([{ "after": a(1, "a"), "text": "x\ny\nz" }]),
        &ctx,
    )
    .await
    .unwrap();
    edit(&path, json!([{ "at": a(4, "c"), "text": "C" }]), &ctx)
        .await
        .unwrap();
    assert_eq!(contents(&path).await, "a\nx\ny\nz\n\nb\nC\n\nd\n");
    let _ = fs::remove_file(path).await;
}

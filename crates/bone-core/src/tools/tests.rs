use std::time::{Duration, Instant};

use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::*;

fn ctx(dir: &tempfile::TempDir) -> ToolContext {
    ToolContext {
        cwd: dir.path().to_owned(),
        session_id: "test".into(),
        cancel: CancellationToken::new(),
    }
}

async fn call(ctx: &ToolContext, name: &str, args: Value) -> ToolResult {
    Registry::builtin().get(name).unwrap().call(args, ctx).await
}

#[tokio::test]
async fn write_read_edit() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(&dir);

    let out = call(
        &ctx,
        "write_file",
        json!({"path": "sub/a.txt", "content": "one\ntwo\ntwo\n"}),
    )
    .await;
    assert!(out.unwrap().starts_with("Created"));

    let out = call(
        &ctx,
        "read_file",
        json!({"path": "sub/a.txt", "offset": 2, "limit": 1}),
    )
    .await
    .unwrap();
    assert_eq!(
        out,
        "     2\ttwo\n\n[showing lines 2-2 of 3; use offset to read more]\n"
    );

    let err = call(
        &ctx,
        "edit_file",
        json!({"path": "sub/a.txt", "old_string": "two", "new_string": "2"}),
    )
    .await;
    assert!(err.unwrap_err().contains("matches 2 places"));
    let err = call(
        &ctx,
        "edit_file",
        json!({"path": "sub/a.txt", "old_string": "zzz", "new_string": "2"}),
    )
    .await;
    assert!(err.unwrap_err().contains("not found"));

    let out = call(
        &ctx,
        "edit_file",
        json!({"path": "sub/a.txt", "old_string": "one", "new_string": "1"}),
    )
    .await;
    assert_eq!(
        out.unwrap(),
        format!(
            "Replaced 1 occurrence in {}",
            dir.path().join("sub/a.txt").display()
        )
    );
    let out = call(
        &ctx,
        "edit_file",
        json!({"path": "sub/a.txt", "old_string": "two", "new_string": "2", "replace_all": true}),
    )
    .await;
    assert!(out.unwrap().starts_with("Replaced 2 occurrences"));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("sub/a.txt")).unwrap(),
        "1\n2\n2\n"
    );
}

#[tokio::test]
async fn bad_arguments_are_errors() {
    let dir = tempfile::tempdir().unwrap();
    let err = call(&ctx(&dir), "read_file", json!({"nope": 1}))
        .await
        .unwrap_err();
    assert!(err.starts_with("invalid arguments"), "{err}");
    assert!(parse_args("{not json").is_err());
    assert_eq!(parse_args("  ").unwrap(), json!({}));
}

#[tokio::test]
async fn shell_combines_output_and_reports_exit_code() {
    let dir = tempfile::tempdir().unwrap();
    let out = call(
        &ctx(&dir),
        "shell",
        json!({"command": "echo out; echo err >&2; pwd; exit 3"}),
    )
    .await
    .unwrap();
    let pwd = std::fs::canonicalize(dir.path()).unwrap();
    assert_eq!(out, format!("out\nerr\n{}\n[exit code: 3]", pwd.display()));
}

#[tokio::test]
async fn shell_does_not_wait_for_background_processes() {
    let dir = tempfile::tempdir().unwrap();
    let start = Instant::now();
    let out = call(
        &ctx(&dir),
        "shell",
        json!({"command": "sleep 30 & echo started"}),
    )
    .await
    .unwrap();
    assert!(start.elapsed() < Duration::from_secs(5));
    assert_eq!(out, "started\n[exit code: 0]");
}

#[tokio::test]
async fn shell_timeout_and_cancel_kill_the_process() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(&dir);
    let start = Instant::now();
    let err = call(
        &ctx,
        "shell",
        json!({"command": "echo hi; sleep 30", "timeout_secs": 1}),
    )
    .await
    .unwrap_err();
    assert!(start.elapsed() < Duration::from_secs(5));
    assert_eq!(err, "hi\n[timed out after 1s; process killed]");

    let cancel = ctx.cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel.cancel();
    });
    let err = call(&ctx, "shell", json!({"command": "sleep 30"}))
        .await
        .unwrap_err();
    assert!(err.ends_with("[cancelled; process killed]"), "{err}");
}

#[test]
fn truncate_middle_respects_char_boundaries() {
    let s = "ééééé"; // 10 bytes
    let t = truncate_middle(s, 3, 3);
    assert!(t.starts_with("é\n") && t.ends_with("\né"), "{t}");
    assert_eq!(truncate_middle("short", 3, 3), "short");
}

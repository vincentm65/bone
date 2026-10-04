use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::*;

fn ctx(dir: &tempfile::TempDir) -> ToolContext {
    ToolContext {
        cwd: dir.path().to_owned(),
        session_id: "test".into(),
        cancel: CancellationToken::new(),
        views: Default::default(),
        output: None,
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
        format!(
            "{}\n\n[showing lines 2-2 of 3; use offset to read more]\n",
            hashline::render_line(2, "two")
        )
    );

    // `old_string` still works, for models used to it.
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
        json!({"path": "sub/./a.txt", "old_string": "two", "new_string": "2", "replace_all": true}),
    )
    .await;
    assert!(out.unwrap().starts_with("Replaced 2 occurrences"));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("sub/a.txt")).unwrap(),
        "one\n2\n2\n"
    );
}

/// `LINE#HASH|text` for line `n` of `text`.
fn anchor(text: &str, n: usize) -> String {
    hashline::render_line(n, text.lines().nth(n - 1).unwrap())
}

/// `LINE#HASH` alone.
fn hash_anchor(text: &str, n: usize) -> String {
    anchor(text, n).split('|').next().unwrap().to_owned()
}

async fn setup(body: &str) -> (tempfile::TempDir, ToolContext, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(&dir);
    let path = dir.path().join("f.rs");
    std::fs::write(&path, body).unwrap();
    call(&ctx, "read_file", json!({"path": "f.rs"}))
        .await
        .unwrap();
    (dir, ctx, path)
}

async fn edit(ctx: &ToolContext, edits: Value) -> ToolResult {
    call(ctx, "edit_file", json!({"path": "f.rs", "edits": edits})).await
}

const SRC: &str = "fn a() {\n    one();\n}\n\nfn b() {\n    two();\n}\n";

#[tokio::test]
async fn anchored_edits_apply_together_and_show_fresh_anchors() {
    let (_d, ctx, path) = setup(SRC).await;
    let out = edit(
        &ctx,
        json!([
            {"at": anchor(SRC, 2), "text": "    uno();\n    dos();"},
            {"after": anchor(SRC, 7), "text": "\nfn c() {}"},
            {"after": "0", "text": "// top"},
        ]),
    )
    .await
    .unwrap();
    let now = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        now,
        "// top\nfn a() {\n    uno();\n    dos();\n}\n\nfn b() {\n    two();\n}\n\nfn c() {}\n"
    );
    let diff: Vec<&str> = out.lines().skip(1).collect();
    let (a, b) = (|n| anchor(&now, n), |n| format!("+{}", anchor(&now, n)));
    assert_eq!(
        diff,
        [
            b(1),
            a(2),
            "-    one();".into(),
            b(3),
            b(4),
            a(5),
            "...".into(),
            a(9),
            b(10),
            b(11),
        ],
        "{out}"
    );

    // Anchors from the first read still work after the edit moved them.
    let out = edit(
        &ctx,
        json!([{"at": hash_anchor(SRC, 6), "text": "    deux();"}]),
    )
    .await
    .unwrap();
    assert!(out.contains("deux"), "{out}");
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("fn b() {\n    deux();\n}")
    );
}

#[tokio::test]
async fn text_rescues_a_miscopied_hash_or_number() {
    let (_d, ctx, path) = setup(SRC).await;
    // Right line, wrong hash; then right text, number one off.
    let out = edit(
        &ctx,
        json!([
            {"at": "2#zz|    one();", "text": "    uno();"},
            {"at": "5|    two();", "text": "    dos();"},
        ]),
    )
    .await
    .unwrap();
    assert!(
        out.contains("found `5|    two();` by its text at line 6"),
        "{out}"
    );
    let now = std::fs::read_to_string(&path).unwrap();
    assert!(now.contains("uno();") && now.contains("dos();"), "{now}");

    // `}` is everywhere: with the number wrong, the text cannot decide.
    let before = std::fs::read_to_string(&path).unwrap();
    let err = edit(&ctx, json!([{"at": "4|}", "text": "};"}]))
        .await
        .unwrap_err();
    assert!(err.contains("could be line 3 or 7"), "{err}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}

#[tokio::test]
async fn a_failed_edit_writes_nothing_and_says_why_per_edit() {
    let (_d, ctx, path) = setup(SRC).await;
    let err = edit(
        &ctx,
        json!([
            {"at": anchor(SRC, 2), "text": "    uno();"},
            {"at": "6#zz", "text": "    dos();"},
        ]),
    )
    .await
    .unwrap_err();
    assert!(err.starts_with("No changes written"), "{err}");
    assert!(err.contains("1 of 2 edits could not be placed"), "{err}");
    assert!(err.contains("edit 2 `6#zz`: line 6 is now"), "{err}");
    assert!(err.contains("The other 1 placed fine"), "{err}");
    assert!(
        err.contains("Tip: write anchors as the whole line"),
        "{err}"
    );
    assert!(err.contains(&anchor(SRC, 6)), "{err}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), SRC);

    let err = edit(&ctx, json!([{"at": "6|    tow();", "text": "x"}]))
        .await
        .unwrap_err();
    assert!(err.contains("the closest line is 6#"), "{err}");
}

#[tokio::test]
async fn changes_made_elsewhere_are_followed_or_refused() {
    let (_d, ctx, path) = setup(SRC).await;
    // Someone adds a line on top and rewrites `one();`.
    let changed = format!("use x;\n{}", SRC.replace("one();", "won();"));
    std::fs::write(&path, &changed).unwrap();
    let out = edit(
        &ctx,
        json!([{"at": hash_anchor(SRC, 6), "text": "    dos();"}]),
    )
    .await
    .unwrap();
    assert!(out.contains("7#"), "the edit followed its line down: {out}");
    let err = edit(
        &ctx,
        json!([{"at": hash_anchor(SRC, 2), "text": "    uno();"}]),
    )
    .await
    .unwrap_err();
    assert!(err.contains("changed since you read them"), "{err}");
}

#[tokio::test]
async fn ranges_over_unseen_lines_are_shown_first() {
    let body: String = (1..=30).map(|i| format!("line {i}\n")).collect();
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(&dir);
    let path = dir.path().join("f.rs");
    std::fs::write(&path, &body).unwrap();
    call(&ctx, "read_file", json!({"path": "f.rs", "limit": 5}))
        .await
        .unwrap();
    call(
        &ctx,
        "read_file",
        json!({"path": "f.rs", "offset": 20, "limit": 5}),
    )
    .await
    .unwrap();
    let range = json!([{"at": anchor(&body, 3), "end": anchor(&body, 22), "text": "gone"}]);
    let err = edit(&ctx, range.clone()).await.unwrap_err();
    assert!(err.contains("lines 3-22 were never shown"), "{err}");
    assert!(err.contains(&anchor(&body, 10)), "{err}");
    // Now they have been shown, the same edit goes through.
    edit(&ctx, range).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap().lines().count(),
        30 - 20 + 1
    );
}

#[tokio::test]
async fn retrying_an_edit_that_went_through_is_not_an_error() {
    let (_d, ctx, path) = setup(SRC).await;
    let edits = json!([{"at": hash_anchor(SRC, 2), "text": "    one_more_time();"}]);
    edit(&ctx, edits.clone()).await.unwrap();
    let out = edit(&ctx, edits).await.unwrap();
    assert!(out.starts_with("No change"), "{out}");
    assert!(out.contains("already in the file"), "{out}");
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("one_more_time")
    );
}

#[tokio::test]
async fn overlapping_edits_are_refused() {
    let (_d, ctx, _path) = setup(SRC).await;
    let err = edit(
        &ctx,
        json!([
            {"at": anchor(SRC, 1), "end": anchor(SRC, 3), "text": "x"},
            {"at": anchor(SRC, 2), "text": "y"},
        ]),
    )
    .await
    .unwrap_err();
    assert!(err.contains("edits 1 and 2 overlap"), "{err}");
}

#[tokio::test]
async fn line_endings_and_a_missing_final_newline_survive() {
    let body = "\u{feff}a\r\nb\r\nc";
    let (_d, ctx, path) = setup(body).await;
    let out = edit(
        &ctx,
        json!([
            {"after": "3#zz|c", "text": "d"},
            {"at": "1|a", "text": "A"},
        ]),
    )
    .await
    .unwrap();
    assert!(out.contains("4#"), "{out}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "\u{feff}A\r\nb\r\nc\r\nd"
    );
}

#[tokio::test]
async fn anchors_work_without_a_read_this_session() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(&dir);
    let path = dir.path().join("f.rs");
    std::fs::write(&path, SRC).unwrap();
    edit(&ctx, json!([{"at": anchor(SRC, 6), "text": "    dos();"}]))
        .await
        .unwrap();
    assert!(std::fs::read_to_string(&path).unwrap().contains("dos();"));
    let err = call(
        &ctx,
        "edit_file",
        json!({"edits": [{"at": "1#aa", "text": ""}]}),
    )
    .await
    .unwrap_err();
    assert!(
        err.contains("missing `path`") && err.contains("f.rs"),
        "{err}"
    );
}

#[tokio::test]
async fn bad_arguments_are_errors() {
    let dir = tempfile::tempdir().unwrap();
    let err = call(&ctx(&dir), "read_file", json!({"nope": 1}))
        .await
        .unwrap_err();
    assert!(err.starts_with("invalid arguments"), "{err}");
    assert!(
        parse_args("{not json")
            .unwrap_err()
            .contains("not valid JSON")
    );
    assert!(
        parse_args(r#"{"path": "a", "edits": [{"#)
            .unwrap_err()
            .contains("cut off")
    );
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

#[tokio::test]
async fn shell_streams_output_while_it_runs() {
    let dir = tempfile::tempdir().unwrap();
    let got = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let sink = got.clone();
    let mut ctx = ctx(&dir);
    ctx.output = Some(std::sync::Arc::new(move |t: &str| {
        sink.lock().unwrap().push(t.to_owned())
    }));
    let out = call(
        &ctx,
        "shell",
        json!({"command": "printf 'a\\n'; sleep 0.2; printf 'b é\\n'"}),
    )
    .await
    .unwrap();
    assert_eq!(out, "a\nb é\n[exit code: 0]");
    let chunks = got.lock().unwrap().clone();
    // The first line came on its own, before the sleep ended.
    assert_eq!(
        chunks.first().map(String::as_str),
        Some("a\n"),
        "{chunks:?}"
    );
    assert_eq!(chunks.concat(), "a\nb é\n");
}

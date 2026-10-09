use std::time::{Duration, Instant};

use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::*;

fn ctx(dir: &tempfile::TempDir) -> ToolContext {
    ToolContext {
        call_id: String::new(),
        cwd: dir.path().to_owned(),
        session_id: "test".into(),
        cancel: CancellationToken::new(),
        jobs: Default::default(),
        output: None,
        processes: None,
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
        "two\n\n[showing lines 2-2 of 3; use offset to read more]\n"
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
async fn shell_short_commands_skip_exit_grace() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(&dir);
    let mut timings = Vec::new();
    for _ in 0..5 {
        let start = Instant::now();
        assert_eq!(
            call(&ctx, "shell", json!({"command": "true"}))
                .await
                .unwrap(),
            "(no output)\n[exit code: 0]"
        );
        timings.push(start.elapsed());
    }
    timings.sort();
    assert!(timings[2] < Duration::from_millis(150), "{timings:?}");
}

#[tokio::test]
async fn shell_preserves_term_cleanup_for_redirected_descendants() {
    let dir = tempfile::tempdir().unwrap();
    let out = call(
        &ctx(&dir),
        "shell",
        json!({"command": r#"python3 -c 'import os, signal, time
from pathlib import Path
def cleanup(*_):
    time.sleep(0.08)
    Path("cleaned").write_text("done")
    os._exit(0)
signal.signal(signal.SIGTERM, cleanup)
Path("ready").touch()
while True: time.sleep(1)
' </dev/null >/dev/null 2>&1 &
while [ ! -f ready ]; do sleep 0.01; done"#, "timeout_secs": 5}),
    )
    .await
    .unwrap();
    assert_eq!(out, "(no output)\n[exit code: 0]");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("cleaned")).unwrap(),
        "done"
    );
}

#[tokio::test]
async fn shell_does_not_wait_for_background_processes() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("leaked");
    let start = Instant::now();
    let out = call(
        &ctx(&dir),
        "shell",
        json!({"command": format!("(sleep 0.3; echo leaked > '{}') & echo started", marker.display())}),
    )
    .await
    .unwrap();
    assert!(start.elapsed() < Duration::from_secs(5));
    assert_eq!(out, "started\n[exit code: 0]");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !marker.exists(),
        "background descendant survived shell exit"
    );
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
async fn plain_read_preserves_line_whitespace() {
    let dir = tempfile::tempdir().unwrap();
    let text = "  leading\tand trailing  \n\tindented\t\n\n \n";
    std::fs::write(dir.path().join("plain.txt"), text).unwrap();
    let out = call(&ctx(&dir), "read_file", json!({"path": "plain.txt"}))
        .await
        .unwrap();
    assert_eq!(out, text);
    let registry = Registry::builtin();
    let spec = registry.get("read_file").unwrap().spec();
    assert!(
        spec.description
            .contains("plain text without per-line prefixes")
    );
    assert_eq!(
        spec.parameters["properties"]["limit"]["description"],
        "Maximum lines to read. Default 2000."
    );
}

#[tokio::test]
async fn plain_read_partial_pagination_and_end_behavior() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(&dir);
    std::fs::write(dir.path().join("plain.txt"), "first\n  second\t\n\nlast\n").unwrap();
    let out = call(
        &ctx,
        "read_file",
        json!({"path": "plain.txt", "offset": 2, "limit": 2}),
    )
    .await
    .unwrap();
    assert_eq!(
        out,
        "  second\t\n\n\n[showing lines 2-3 of 4; use offset to read more]\n"
    );
    let out = call(
        &ctx,
        "read_file",
        json!({"path": "plain.txt", "offset": 4, "limit": 2}),
    )
    .await
    .unwrap();
    assert_eq!(out, "last\n");
    let err = call(&ctx, "read_file", json!({"path": "plain.txt", "offset": 5}))
        .await
        .unwrap_err();
    assert_eq!(err, "offset 5 is past the end of the file (4 lines)");
    let out = call(
        &ctx,
        "read_file",
        json!({"path": "plain.txt", "offset": 0, "limit": 0}),
    )
    .await
    .unwrap();
    assert_eq!(
        out,
        "first\n\n[showing lines 1-1 of 4; use offset to read more]\n"
    );
}

#[tokio::test]
async fn plain_read_keeps_historical_newline_rendering() {
    let dir = tempfile::tempdir().unwrap();
    for (input, expected) in [
        ("a\r\nb\r\n", "a\nb\n"),
        ("a\nb", "a\nb\n"),
        ("a\rb\r", "a\rb\r\n"),
        ("\n", "\n"),
    ] {
        std::fs::write(dir.path().join("plain.txt"), input).unwrap();
        let out = call(&ctx(&dir), "read_file", json!({"path": "plain.txt"}))
            .await
            .unwrap();
        assert_eq!(out, expected);
    }
}

#[tokio::test]
async fn plain_read_empty_binary_and_lossy_utf8() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(&dir);
    std::fs::write(dir.path().join("empty.txt"), "").unwrap();
    let out = call(
        &ctx,
        "read_file",
        json!({"path": "empty.txt", "offset": 99}),
    )
    .await
    .unwrap();
    assert_eq!(out, "(empty file)");
    std::fs::write(dir.path().join("binary.txt"), b"a\0b").unwrap();
    let err = call(&ctx, "read_file", json!({"path": "binary.txt"}))
        .await
        .unwrap_err();
    assert_eq!(
        err,
        format!(
            "{} looks like a binary file",
            dir.path().join("binary.txt").display()
        )
    );
    std::fs::write(dir.path().join("lossy.txt"), b"a\xffb\n").unwrap();
    let out = call(&ctx, "read_file", json!({"path": "lossy.txt"}))
        .await
        .unwrap();
    assert_eq!(out, "a\u{fffd}b\n");
}

#[tokio::test]
async fn plain_read_utf8_long_line_limit_is_2000_chars() {
    let dir = tempfile::tempdir().unwrap();
    let exact = "é🦀".repeat(1000);
    std::fs::write(dir.path().join("long.txt"), format!("{exact}\n{exact}終\n")).unwrap();
    let out = call(&ctx(&dir), "read_file", json!({"path": "long.txt"}))
        .await
        .unwrap();
    assert_eq!(out, format!("{exact}\n{exact} [line truncated]\n"));
}

#[tokio::test]
async fn plain_read_default_limit_is_2000_lines() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(&dir);
    let first = "line\n".repeat(2000);
    let text = format!("{first}last\n");
    std::fs::write(dir.path().join("many.txt"), &text).unwrap();
    let out = call(&ctx, "read_file", json!({"path": "many.txt"}))
        .await
        .unwrap();
    assert_eq!(
        out,
        format!("{first}\n[showing lines 1-2000 of 2001; use offset to read more]\n")
    );
    let out = call(
        &ctx,
        "read_file",
        json!({"path": "many.txt", "offset": 2001}),
    )
    .await
    .unwrap();
    assert_eq!(out, "last\n");
    let out = call(
        &ctx,
        "read_file",
        json!({"path": "many.txt", "limit": 2001}),
    )
    .await
    .unwrap();
    assert_eq!(out, text);
}

#[tokio::test]
async fn plain_read_to_exact_edit_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(&dir);
    let original = "  α\t\n\n\tβ  \n";
    let updated = "  α\t\n\n\tchanged β  \n";
    std::fs::write(dir.path().join("roundtrip.txt"), original).unwrap();
    let read = call(&ctx, "read_file", json!({"path": "roundtrip.txt"}))
        .await
        .unwrap();
    assert_eq!(read, original);
    let out = call(
        &ctx,
        "edit_file",
        json!({"path": "roundtrip.txt", "old_string": read, "new_string": updated}),
    )
    .await
    .unwrap();
    assert!(out.starts_with("Replaced 1 occurrence"));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("roundtrip.txt")).unwrap(),
        updated
    );
    let read = call(
        &ctx,
        "read_file",
        json!({"path": "roundtrip.txt", "offset": 3, "limit": 1}),
    )
    .await
    .unwrap();
    assert_eq!(read, "\tchanged β  \n");
    call(
        &ctx,
        "edit_file",
        json!({"path": "roundtrip.txt", "old_string": read, "new_string": "\tfinal β  \n"}),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("roundtrip.txt")).unwrap(),
        "  α\t\n\n\tfinal β  \n"
    );
}

#[tokio::test]
async fn batch_validation_failure_never_writes() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(&dir);
    let path = dir.path().join("a.txt");
    let original = "alpha beta gamma\r\n";
    std::fs::write(&path, original).unwrap();
    for edits in [
        json!([{"old_string":"alpha", "new_string":"A"}, {"old_string":"missing", "new_string":"B"}]),
        json!([{"old_string":"alpha beta", "new_string":"A"}, {"old_string":"beta", "new_string":"B"}]),
        json!([{"old_string":"alpha", "new_string":"A"}, {"old_string":"beta", "new_string":"beta"}]),
        json!([{"old_string":"alpha", "new_string":"A"}, {"old_string":"", "new_string":"B"}]),
        json!([{"old_string":"alpha", "new_string":"A"}, {"old_string":"alpha", "new_string":"B"}]),
        json!([{"old_string":"alpha", "new_string":"A"}, {"old_string":"beta", "new_string":"B", "typo":true}]),
    ] {
        assert!(
            call(&ctx, "edit_file", json!({"path":"a.txt", "edits": edits}))
                .await
                .is_err()
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }
    let out = call(
        &ctx,
        "edit_file",
        json!({"path":"a.txt", "edits":[
            {"old_string":"gamma", "new_string":"G"},
            {"old_string":"alpha", "new_string":"longer alpha"}
        ]}),
    )
    .await
    .unwrap();
    assert!(out.contains("2 edits"));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "longer alpha beta G\r\n"
    );
}

#[tokio::test]
async fn plain_read_huge_limit_does_not_overflow() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("plain.txt"), "first\nsecond\nlast").unwrap();
    let out = call(
        &ctx(&dir),
        "read_file",
        json!({"path": "plain.txt", "offset": 2, "limit": usize::MAX}),
    )
    .await
    .unwrap();
    assert_eq!(out, "second\nlast\n");
}

#[tokio::test]
async fn exact_edit_rejects_non_utf8_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.txt");
    let original = b"original\xff\n";
    std::fs::write(&path, original).unwrap();
    let err = call(
        &ctx(&dir),
        "edit_file",
        json!({"path":"a.txt", "edits":[
            {"old_string":"original", "new_string":"new"}
        ]}),
    )
    .await
    .unwrap_err();
    assert!(err.starts_with("cannot read"), "{err}");
    assert_eq!(std::fs::read(&path).unwrap(), original);
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

#[tokio::test]
async fn shell_manages_background_jobs_and_reports_completion() {
    let dir = tempfile::tempdir().unwrap();
    let mut ctx = ctx(&dir);
    let updates = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = updates.clone();
    ctx.processes = Some(std::sync::Arc::new(move |view, _| {
        seen.lock().unwrap().push(view);
    }));

    let started = call(
        &ctx,
        "shell",
        json!({"command": "printf first; sleep 0.1; printf second", "mode": "start"}),
    )
    .await
    .unwrap();
    let id = started
        .strip_prefix("background process started: ")
        .unwrap();
    let status = call(&ctx, "shell", json!({"action": "status", "id": id}))
        .await
        .unwrap();
    assert!(status.contains(id));

    let result = call(&ctx, "shell", json!({"action": "wait", "id": id}))
        .await
        .unwrap();
    assert_eq!(result, "firstsecond\n[exit code: 0]");

    for _ in 0..20 {
        if updates
            .lock()
            .unwrap()
            .iter()
            .any(|view| view.state == ProcessState::Exited)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        updates
            .lock()
            .unwrap()
            .iter()
            .any(|view| view.state == ProcessState::Exited && view.tail.contains("first"))
    );
}

#[tokio::test]
async fn background_jobs_run_in_a_terminal_and_the_model_reads_clean_text() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(&dir);
    let started = call(
        &ctx,
        "shell",
        json!({
            "command": "printf '\\033[32mok\\033[0m\\n'; printf '10%%\\r50%%\\r100%%\\n'; [ -t 1 ] && echo tty; echo $TERM $GIT_PAGER",
            "mode": "start",
        }),
    )
    .await
    .unwrap();
    let id = started
        .strip_prefix("background process started: ")
        .unwrap();
    let done = call(&ctx, "shell", json!({"action": "wait", "id": id}))
        .await
        .unwrap();
    assert_eq!(done, "ok\n100%\ntty\nxterm-256color cat\n[exit code: 0]");
    // The terminal's own bytes are kept for terminal views.
    let (offset, raw, total) = ctx.jobs.read(&ctx.session_id, id, 0).unwrap();
    assert_eq!(offset, 0);
    assert_eq!(total, raw.len() as u64);
    assert!(raw.starts_with("\u{1b}[32mok\u{1b}[0m\r\n"), "{raw:?}");
    let (offset, rest, _) = ctx.jobs.read(&ctx.session_id, id, 4).unwrap();
    assert_eq!((offset, rest.as_str()), (4, &raw[4..]));
}

#[test]
fn trim_front_cuts_in_batches_on_char_boundaries() {
    let mut s = "é".repeat(5); // 10 bytes
    // Up to a quarter past the cap is left alone.
    assert_eq!(shell::trim_front(&mut s, 8), 0);
    s.push_str("abc");
    // Past it, back to about the cap, never inside a character.
    assert_eq!(shell::trim_front(&mut s, 8), 6);
    assert_eq!(s, "ééabc");
}

#[test]
fn the_cleaner_settles_progress_lines_and_drops_escapes() {
    let mut c = shell::Cleaner::default();
    let mut out = String::new();
    // Split mid-escape and mid-CRLF, as reads can be.
    for part in [
        "a\u{1b}[3",
        "1mred\u{1b}[0m\r",
        "\nbar 1\rbar 2\rbar 3\r\n",
        "x\u{8}y",
        "\u{1b}]0;title\u{7}z\n",
    ] {
        c.apply(part, &mut out);
    }
    assert_eq!(out, "ared\nbar 3\nyz\n");
}

#[tokio::test]
async fn shell_background_job_survives_turn_cancellation() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(&dir);
    let cancel = ctx.cancel.clone();
    let started = call(
        &ctx,
        "shell",
        json!({"command": "sleep 1", "mode": "start"}),
    )
    .await
    .unwrap();
    let id = started
        .strip_prefix("background process started: ")
        .unwrap();
    cancel.cancel();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let status = call(&ctx, "shell", json!({"action": "status", "id": id}))
        .await
        .unwrap();
    assert!(status.contains("\"running\": true"), "{status}");
    call(&ctx, "shell", json!({"action": "kill", "id": id}))
        .await
        .unwrap();
}

#[tokio::test]
async fn shell_status_bounds_saved_output_for_the_model() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(&dir);
    let started = call(
        &ctx,
        "shell",
        json!({"command": "printf '%*s' 100000 '' | tr ' ' x", "mode": "start"}),
    )
    .await
    .unwrap();
    let id = started
        .strip_prefix("background process started: ")
        .unwrap();
    let _ = call(&ctx, "shell", json!({"action": "wait", "id": id}))
        .await
        .unwrap();
    let status = call(&ctx, "shell", json!({"action": "status", "id": id}))
        .await
        .unwrap();
    let status: Value = serde_json::from_str(&status).unwrap();
    assert!(status["output"].as_str().unwrap().len() < 40_000);
    assert_eq!(status["output_bytes"], 100_000);
    assert_eq!(status["truncated"], true);
}

#[tokio::test]
async fn shell_kills_a_managed_background_job() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(&dir);
    let started = call(
        &ctx,
        "shell",
        json!({"command": "sleep 30", "mode": "start"}),
    )
    .await
    .unwrap();
    let id = started
        .strip_prefix("background process started: ")
        .unwrap();
    call(&ctx, "shell", json!({"action": "kill", "id": id}))
        .await
        .unwrap();
    let result = call(&ctx, "shell", json!({"action": "wait", "id": id}))
        .await
        .unwrap_err();
    assert!(
        result.contains("cancelled") || result.contains("signal"),
        "{result}"
    );
}

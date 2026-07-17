mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use serde_json::json;

use bone_core::processes;
use bone_core::tools::shell::ShellTool;
use bone_core::tools::shell::truncate_output;
use bone_core::tools::types::{Tool, ToolExecutionContext};
#[tokio::test]
async fn timeout_returns_partial_stdout() {
    // A command that prints to stdout then sleeps past the timeout. The model
    // must still receive the partial output so it can decide next steps.
    let tool = ShellTool;

    let result = tool
        .execute(json!({
            "command": "echo 'partial compiler output' && sleep 5",
            "timeout_ms": 1000
        }))
        .await;

    assert!(result.is_err(), "expected timeout error, got: {result:?}");
    let err = result.unwrap_err();
    assert!(err.contains("[timed out after 1000ms; partial output]"));
    assert!(err.contains("partial compiler output"));
}

#[tokio::test]
async fn successful_command_returns_exit_code_and_stdout() {
    let tool = ShellTool;

    let result = tool
        .execute(json!({ "command": "echo hello" }))
        .await
        .expect("command should succeed");

    assert!(result.contains("exit code: 0"));
    assert!(result.contains("hello"));
}

#[tokio::test]
async fn live_foreground_uses_context_working_directory() {
    let cwd = common::temp_path("shell-foreground-cwd");
    tokio::fs::create_dir_all(&cwd).await.unwrap();
    let output = ShellTool
        .execute_output_live(
            json!({ "command": "pwd" }),
            None,
            ToolExecutionContext::default().with_working_dir(cwd.clone()),
        )
        .await
        .expect("pwd should succeed");
    assert!(output.content.contains(&cwd.to_string_lossy().to_string()));
    let _ = tokio::fs::remove_dir_all(cwd).await;
}

#[tokio::test]
async fn live_background_uses_context_working_directory() {
    let cwd = common::temp_path("shell-background-cwd");
    tokio::fs::create_dir_all(&cwd).await.unwrap();
    let output = ShellTool
        .execute_output_live(
            json!({ "command": "pwd", "background": true }),
            None,
            ToolExecutionContext::default().with_working_dir(cwd.clone()),
        )
        .await
        .expect("background pwd should start");
    let id = output
        .content
        .strip_prefix("background process started: ")
        .unwrap();
    let snapshot = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let snapshot = processes::registry().get(id).unwrap();
            if !snapshot.running {
                break snapshot;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("background pwd should finish");
    assert_eq!(snapshot.stdout.trim(), cwd.to_string_lossy());
    let _ = tokio::fs::remove_dir_all(cwd).await;
}

#[tokio::test]
async fn live_shell_rejects_nul_bytes() {
    let error = ShellTool
        .execute_output_live(
            json!({ "command": "echo before\0after" }),
            None,
            ToolExecutionContext::default(),
        )
        .await
        .expect_err("live shell should reject NUL bytes");
    assert!(error.contains("must not contain NUL bytes"));
}

#[test]
fn truncates_single_huge_line() {
    let output = truncate_output(&"x".repeat(10 * 1024 * 1024), 500);
    assert!(output.contains("…[truncated]"));
    assert!(output.len() < 10_000, "output len was {}", output.len());
}

#[tokio::test]
async fn classification_is_accepted_but_ignored() {
    // Old transcripts/providers still send classification; the schema no longer
    // requires it but the arg must still deserialize without error.
    let tool = ShellTool;

    let result = tool
        .execute(json!({
            "command": "echo ok",
            "classification": "read_only"
        }))
        .await
        .expect("command should succeed");

    assert!(result.contains("exit code: 0"));
}

#[tokio::test]
async fn obvious_shell_file_writes_redirect_to_dedicated_tools() {
    let tool = ShellTool;
    for command in [
        "sed -i 's/old/new/' file.txt",
        "sed -ibak 's/old/new/' file.txt",
        "sed --in-place=.bak 's/old/new/' file.txt",
        "tee file.txt",
        "printf hello > file.txt",
        "echo hello | tee file.txt",
        "cat source.txt > copy.txt",
    ] {
        let error = tool
            .execute(json!({ "command": command }))
            .await
            .expect_err(command);
        assert!(
            error.contains("write_file") || error.contains("edit_file"),
            "{command}: {error}"
        );
    }
}

#[tokio::test]
async fn shell_keeps_read_fallbacks_and_normal_commands_available() {
    let tool = ShellTool;
    for command in ["cat /dev/null", "head -1 /dev/null", "printf hello"] {
        let result = tool.execute(json!({ "command": command })).await;
        assert!(result.is_ok(), "{command}: {result:?}");
    }
}

#[tokio::test]
async fn cancel_kills_promptly_and_returns_partial_output() {
    // A command that prints then sleeps well past the wall-clock timeout. We
    // flip the cancel flag mid-run and expect execute_output_live to return
    // within a second or so (far under the 30s timeout) with partial output —
    // proving the process tree is killed on cancel, not waited out.
    let tool = ShellTool;
    let cancel = Arc::new(AtomicBool::new(false));
    let mut ctx = ToolExecutionContext::default();
    ctx.cancelled = Some(cancel.clone());

    let cmd = tokio::spawn(async move {
        tool.execute_output_live(
            json!({
                "command": "echo 'starting download' && sleep 30",
                "timeout_ms": 30_000
            }),
            None,
            ctx,
        )
        .await
    });

    // Let the echo land, then cancel.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let start = Instant::now();
    cancel.store(true, Ordering::Relaxed);

    let result = cmd.await.expect("task panicked");
    let elapsed = start.elapsed();

    assert!(result.is_err(), "expected cancel error, got: {result:?}");
    let err = result.unwrap_err();
    assert!(err.contains("[cancelled by user"));
    assert!(err.contains("starting download"));
    // Must return in well under the 30s timeout — the polling interval is 25ms.
    assert!(
        elapsed.as_secs() < 5,
        "cancel took {elapsed:?}, expected < 5s"
    );
}

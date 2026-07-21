mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use serde_json::json;

use bone_core::processes;
use bone_core::tools::builtin_tools;
use bone_core::tools::shell::{ScriptRequest, ShellTool, run_script_lines, truncate_output};
use bone_core::tools::types::{Tool, ToolExecutionContext};

#[test]
fn process_lifecycle_is_exposed_only_through_shell() {
    let definitions = builtin_tools().definitions();
    assert!(!definitions.iter().any(|tool| tool.name == "process"));
    let shell = definitions
        .iter()
        .find(|tool| tool.name == "shell")
        .expect("shell should be registered");
    assert_eq!(
        shell.input_schema["properties"]["action"]["enum"],
        json!(["run", "list", "status", "kill"])
    );
}

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
async fn shell_manages_its_background_processes() {
    let started = ShellTool
        .execute(json!({ "action": "run", "command": "sleep 30", "background": true }))
        .await
        .expect("background command should start");
    let id = started
        .strip_prefix("background process started: ")
        .expect("result should contain a process id");

    let listed = ShellTool
        .execute(json!({ "action": "list" }))
        .await
        .expect("list should succeed");
    assert!(listed.contains(id));

    let status = ShellTool
        .execute(json!({ "action": "status", "id": id }))
        .await
        .expect("status should succeed");
    assert!(status.contains("running: true"));

    let killed = ShellTool
        .execute(json!({ "action": "kill", "id": id }))
        .await
        .expect("kill should succeed");
    assert_eq!(killed, format!("stop requested for {id}"));

    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while processes::registry().get(id).unwrap().running {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("killed process should stop");
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
    let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut ctx = ToolExecutionContext::default();
    ctx.call_id = "cancel-test".into();
    ctx.cancelled = Some(cancel.clone());
    ctx.runtime_events = Some(events_tx);

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

    // Wait until live output carries the echo, then cancel.
    let saw_output = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match events_rx.recv().await {
                Some(bone_core::runtime::RuntimeEvent::ToolOutput { content, .. })
                    if content.contains("starting download") =>
                {
                    break;
                }
                Some(_) => continue,
                None => panic!("event channel closed before shell output"),
            }
        }
    })
    .await;
    assert!(
        saw_output.is_ok(),
        "timed out waiting for live shell output before cancel"
    );

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

#[tokio::test(flavor = "multi_thread")]
async fn cancellation_does_not_invoke_callbacks_for_buffered_output() {
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_after_first = cancel.clone();
    let (first_line_tx, first_line_rx) = std::sync::mpsc::channel();
    let cancel_thread = std::thread::spawn(move || {
        first_line_rx.recv().unwrap();
        cancel_after_first.store(true, Ordering::Relaxed);
    });
    let mut first_line_tx = Some(first_line_tx);
    let mut lines = Vec::new();

    let result = run_script_lines(
        ScriptRequest {
            command: "printf 'first\\n'; sleep 0.05; printf 'second\\n'; sleep 30".into(),
            env: Vec::new(),
            timeout_ms: 30_000,
            working_dir: None,
            cancel: Some(cancel),
        },
        |line| {
            lines.push(line);
            if let Some(tx) = first_line_tx.take() {
                tx.send(()).unwrap();
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            Ok(())
        },
    )
    .await;
    cancel_thread.join().unwrap();

    let error = match result {
        Ok(_) => panic!("command should be cancelled"),
        Err(error) => error,
    };
    assert!(error.contains("cancelled by user"));
    assert_eq!(lines, ["first"]);
}

use bone::tools::script_runner::{ScriptRequest, run_script_jsonl};
use std::sync::{Arc, Mutex};

#[tokio::test]
async fn jsonl_runner_drains_stderr_while_reading_stdout() {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&lines);
    let output = run_script_jsonl(
        ScriptRequest {
            command: r#"i=0; while [ "$i" -lt 20000 ]; do echo stderr-line >&2; i=$((i+1)); done; echo '{"type":"finished","content":"ok"}'"#.to_string(),
            env: Vec::new(),
            timeout_ms: 2_000,
        },
        move |line| {
            seen.lock().unwrap().push(line);
        },
    )
    .await
    .expect("jsonl runner should not deadlock on large stderr output");

    assert_eq!(output.exit_code, Some(0));
    assert!(output.stdout.contains(r#""content":"ok""#));
    assert!(!output.stderr.is_empty());
    assert_eq!(lines.lock().unwrap().len(), 1);
}

use super::*;

#[test]
fn output_capture_keeps_both_ends_with_a_fixed_memory_limit() {
    let mut capture = OutputCapture::new();
    capture.push(&vec![b'a'; CAPTURE_BYTES]);
    capture.push(&vec![b'z'; CAPTURE_BYTES]);

    let output = capture.render(500);
    assert!(output.starts_with('a'));
    assert!(
        output
            .lines()
            .last()
            .is_some_and(|line| line.starts_with('z'))
    );
    assert!(output.contains("bytes truncated"));
    assert!(output.len() < 10_000);
}

#[test]
fn shell_timeout_defaults_to_five_minutes() {
    let args = parse_shell_args(serde_json::json!({ "command": "true" })).unwrap();
    let (_, timeout_ms, background) = parse_run_args(args).unwrap();

    assert_eq!(timeout_ms, 300_000);
    assert!(!background);
}

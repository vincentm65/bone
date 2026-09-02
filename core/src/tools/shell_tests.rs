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

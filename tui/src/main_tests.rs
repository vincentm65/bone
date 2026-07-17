use super::cli::{approval_mode, has_flag, parse_provider_model};
use bone::tools::ApprovalMode;

fn args(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

#[test]
fn parse_provider_model_extracts_both() {
    let (p, m) = parse_provider_model(&args(&[
        "--listen",
        "127.0.0.1:7878",
        "--provider",
        "codex",
        "--model",
        "gpt-5.5",
    ]));
    assert_eq!(p.as_deref(), Some("codex"));
    assert_eq!(m.as_deref(), Some("gpt-5.5"));
}

#[test]
fn parse_provider_model_ignores_unknown_and_missing() {
    // Unlike `parse_cli_options`, unknown flags (e.g. `--listen`) are ignored
    // rather than rejected, and absent flags yield `None`.
    let (p, m) = parse_provider_model(&args(&["--listen", "x", "--verbose"]));
    assert!(p.is_none());
    assert!(m.is_none());
}

#[test]
fn has_flag_detects_presence() {
    assert!(has_flag(
        &args(&["--shutdown-on-stdin-eof"]),
        "--shutdown-on-stdin-eof"
    ));
    assert!(!has_flag(
        &args(&["--listen", "x"]),
        "--shutdown-on-stdin-eof"
    ));
}

#[test]
fn approval_mode_uses_canonical_setting() {
    assert_eq!(approval_mode("danger"), ApprovalMode::Danger);
    assert_eq!(approval_mode("safe"), ApprovalMode::Safe);
    assert_eq!(approval_mode("invalid"), ApprovalMode::Safe);
}

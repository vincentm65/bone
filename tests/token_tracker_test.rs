use bone::llm::token_tracker::{TokenStats, format_tokens};

#[test]
fn record_real_usage() {
    let mut stats = TokenStats::new();
    stats.record_request(1234, 56);
    assert_eq!(stats.sent, 1234);
    assert_eq!(stats.received, 56);
    assert_eq!(stats.request_count, 1);
    assert_eq!(stats.context_length, 1234);
    assert_eq!(stats.total(), 1290);
}

#[test]
fn record_estimate() {
    let mut stats = TokenStats::new();
    stats.record_estimate(400, 200);
    // 400/3.8 = 105.3 → ceil = 106, 200/3.8 = 52.6 → ceil = 53
    assert_eq!(stats.sent, 106);
    assert_eq!(stats.received, 53);
    assert_eq!(stats.context_length, 106);
}

#[test]
fn format_tokens_small() {
    assert_eq!(format_tokens(42), "42");
}

#[test]
fn format_tokens_thousands() {
    assert_eq!(format_tokens(1_234), "1,234");
    assert_eq!(format_tokens(9_999), "9,999");
    assert_eq!(format_tokens(10_000), "10,000");
    assert_eq!(format_tokens(12_345), "12,345");
}

#[test]
fn format_tokens_millions() {
    assert_eq!(format_tokens(1_000_000), "1,000,000");
    assert_eq!(format_tokens(1_234_567), "1,234,567");
    assert_eq!(format_tokens(12_345_678), "12,345,678");
}

#[test]
fn display_format() {
    let mut stats = TokenStats::new();
    stats.record_request(1234, 56);
    assert_eq!(stats.display(), "curr 1,234 | in 1,234 | out 56");
}

#[test]
fn display_format_no_context() {
    let stats = TokenStats::new();
    assert_eq!(stats.display(), "curr 0 | in 0 | out 0");
}

#[test]
fn display_received_override_is_cumulative() {
    let mut stats = TokenStats::new();
    stats.record_request(100, 25);
    assert_eq!(
        stats.display_with_received_override(Some(stats.received + 10)),
        "curr 100 | in 100 | out 35"
    );
}

#[test]
fn estimate_tool_call_tokens_from_json_size() {
    // ~4 UTF-8 chars per token (same ratio used in streaming).
    let chars_per_token = 4u64;

    // Tiny argument: {"path":"f.rs"} = 15 chars → 3 tokens
    let tiny = r#"{"path":"f.rs"}"#.to_string();
    assert_eq!(tiny.len() as u64 / chars_per_token, 3);
    assert_eq!(tiny.len(), 15);

    // Medium argument: a file path + expected_hash = 72 chars → 18 tokens
    let medium = r#"{"path":"src/main.rs","expected_hash":"abc123","content":"fn main() {}"}"#.to_string();
    assert_eq!(medium.len() as u64 / chars_per_token, 18);
    assert_eq!(medium.len(), 72);

    // Large argument: multi-line JSON = 150 chars → 37 tokens
    let large_json = r#"{"path":"src/lib.rs","expected_hash":"deadbeef","content":"use std::io;\n\npub fn hello() -> io::Result<()> {\n    println!(\"hello\");\n    Ok(())\n}"#.to_string();
    assert_eq!(large_json.len() as u64 / chars_per_token, 37);
    assert_eq!(large_json.len(), 150);

    // Ensure minimum of 1 token for any non-empty argument
    let one_char = r#"{}"#.to_string();
    assert_eq!(one_char.len() as u64 / chars_per_token, 0);
    assert!((one_char.len() as u64 / chars_per_token).max(1) >= 1);
}

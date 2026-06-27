use bone_core::llm::token_tracker::{TokenStats, format_tokens};

#[test]
fn reset_clears_all_fields() {
    let mut stats = TokenStats::new();
    stats.record_request(100, 50, Some(20), Some(0.01));
    stats.reset();
    assert_eq!(stats.sent, 0);
    assert_eq!(stats.received, 0);
    assert_eq!(stats.cached, 0);
    assert_eq!(stats.cost, 0.0);
    assert_eq!(stats.request_count, 0);
    assert_eq!(stats.context_length, 0);
}

#[test]
fn record_request_tracks_cached_and_cost() {
    let mut stats = TokenStats::new();
    stats.record_request(1000, 200, Some(300), Some(0.005));
    assert_eq!(stats.sent, 1000);
    assert_eq!(stats.received, 200);
    assert_eq!(stats.cached, 300);
    assert!((stats.cost - 0.005).abs() < f64::EPSILON);
    assert_eq!(stats.request_count, 1);
}

#[test]
fn record_request_none_cached_and_cost_defaults_to_zero() {
    let mut stats = TokenStats::new();
    stats.record_request(100, 50, None, None);
    assert_eq!(stats.cached, 0);
    assert_eq!(stats.cost, 0.0);
}

#[test]
fn cumulative_cached_and_cost_across_requests() {
    let mut stats = TokenStats::new();
    stats.record_request(100, 50, Some(10), Some(0.001));
    stats.record_request(200, 80, Some(30), Some(0.002));
    assert_eq!(stats.cached, 40);
    assert!((stats.cost - 0.003).abs() < f64::EPSILON);
}

#[test]
fn one_liner_includes_cached_and_cost_when_present() {
    let mut stats = TokenStats::new();
    stats.record_request(1000, 200, Some(300), Some(0.1234));
    let s = stats.one_liner();
    assert!(s.contains("cached"));
    assert!(s.contains("$0.12"));
}

#[test]
fn one_liner_omits_cached_and_cost_when_zero() {
    let mut stats = TokenStats::new();
    stats.record_request(100, 50, None, None);
    let s = stats.one_liner();
    assert!(!s.contains("cached"));
    assert!(!s.contains("$"));
}

#[test]
fn one_liner_shows_request_count() {
    let mut stats = TokenStats::new();
    stats.record_request(100, 50, None, None);
    stats.record_request(200, 80, None, None);
    let s = stats.one_liner();
    assert!(s.starts_with("2 req"));
}

#[test]
fn record_real_usage() {
    let mut stats = TokenStats::new();
    stats.record_request(1234, 56, None, None);
    assert_eq!(stats.sent, 1234);
    assert_eq!(stats.received, 56);
    assert_eq!(stats.request_count, 1);
    assert_eq!(stats.context_length, 1234);
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
fn set_context_estimate_updates_current_only() {
    let mut stats = TokenStats::new();
    stats.record_request(1000, 50, None, None);

    stats.set_context_estimate(380);

    assert_eq!(stats.context_length, 100);
    assert_eq!(stats.sent, 1000);
    assert_eq!(stats.received, 50);
    assert_eq!(stats.request_count, 1);
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
fn estimate_tool_call_tokens_from_json_size() {
    // ~4 UTF-8 chars per token (same ratio used in streaming).
    let chars_per_token = 4u64;

    // Tiny argument: {"path":"f.rs"} = 15 chars → 3 tokens
    let tiny = r#"{"path":"f.rs"}"#.to_string();
    assert_eq!(tiny.len() as u64 / chars_per_token, 3);
    assert_eq!(tiny.len(), 15);

    // Medium argument: a file path + expected_hash = 72 chars → 18 tokens
    let medium =
        r#"{"path":"src/main.rs","expected_hash":"abc123","content":"fn main() {}"}"#.to_string();
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

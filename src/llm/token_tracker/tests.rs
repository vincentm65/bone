use super::*;

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

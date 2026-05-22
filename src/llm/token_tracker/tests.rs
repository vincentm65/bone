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
    // 400/4 = 100, 200/4 = 50
    assert_eq!(stats.sent, 100);
    assert_eq!(stats.received, 50);
    assert_eq!(stats.context_length, 100);
}

#[test]
fn format_tokens_small() {
    assert_eq!(format_tokens(42), "42");
}

#[test]
fn format_tokens_thousands() {
    assert_eq!(format_tokens(1234), "1234");
    assert_eq!(format_tokens(9999), "9999");
    assert_eq!(format_tokens(10_000), "10.0k");
    assert_eq!(format_tokens(12_345), "12.3k");
}

#[test]
fn format_tokens_millions() {
    assert_eq!(format_tokens(12_345_678), "12.3M");
}

#[test]
fn display_format() {
    let mut stats = TokenStats::new();
    stats.record_request(1234, 56);
    assert_eq!(stats.display(), "curr: 1234 in: 1234 out: 56");
}

#[test]
fn display_format_no_context() {
    let stats = TokenStats::new();
    assert_eq!(stats.display(), "curr: 0 in: 0 out: 0");
}

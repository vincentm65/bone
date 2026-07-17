use super::*;

#[test]
fn anchored_estimate_without_anchor_is_raw_guess() {
    let stats = TokenStats::new();
    assert_eq!(stats.anchored_context_estimate(38), 10);
}

#[test]
fn anchored_estimate_adds_growth_to_reported_tokens() {
    let mut stats = TokenStats::new();
    // Provider reported 50_000 tokens for a request we estimated at
    // 100_000 chars (raw guess would say ~26_316 — far off).
    stats.set_context_anchor(50_000, 100_000);
    assert_eq!(stats.anchored_context_estimate(100_000), 50_000);
    assert_eq!(stats.anchored_context_estimate(100_038), 50_010);
}

#[test]
fn anchored_estimate_handles_small_shrink() {
    let mut stats = TokenStats::new();
    stats.set_context_anchor(50_000, 100_000);
    // A dropped transient turn message shrinks chars slightly; stay on
    // the anchored scale instead of reverting to the raw guess.
    assert_eq!(stats.anchored_context_estimate(99_962), 49_990);
}

#[test]
fn reset_and_clear_drop_the_anchor() {
    let mut stats = TokenStats::new();
    stats.set_context_anchor(50_000, 100_000);
    stats.clear_context_anchor();
    assert_eq!(stats.anchored_context_estimate(38_000), 10_000);
    stats.set_context_anchor(50_000, 100_000);
    stats.reset();
    assert!(stats.context_anchor.is_none());
}

#[test]
fn records_and_resets_usage() {
    let mut stats = TokenStats::new();
    stats.record_request(1_000, 200, Some(300), Some(0.005));
    stats.record_request(200, 80, None, None);
    assert_eq!(
        (stats.sent, stats.received, stats.cached),
        (1_200, 280, 300)
    );
    assert_eq!(stats.context_length, 200);
    assert_eq!(stats.request_count, 2);
    assert!((stats.cost - 0.005).abs() < f64::EPSILON);

    stats.reset();
    assert_eq!(
        (
            stats.sent,
            stats.received,
            stats.cached,
            stats.cost,
            stats.request_count,
            stats.context_length,
            stats.context_anchor,
        ),
        (0, 0, 0, 0.0, 0, 0, None)
    );
}

#[test]
fn estimates_usage_and_current_context() {
    let mut stats = TokenStats::new();
    stats.record_estimate(400, 200);
    assert_eq!(
        (stats.sent, stats.received, stats.context_length),
        (106, 53, 106)
    );
    stats.set_context_estimate(380);
    assert_eq!(stats.context_length, 100);
    assert_eq!((stats.sent, stats.received), (106, 53));
}

#[test]
fn formats_counts_and_optional_summary_fields() {
    assert_eq!(format_tokens(1_234_567), "1,234,567");
    let mut stats = TokenStats::new();
    stats.record_request(1_000, 200, None, None);
    assert_eq!(stats.one_liner(), "1 req | 1,000 in | 200 out");
    stats.record_request(1, 1, Some(300), Some(0.125));
    let summary = stats.one_liner();
    assert!(summary.contains("300 cached"));
    assert!(summary.contains("$0.12"));
}

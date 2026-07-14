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

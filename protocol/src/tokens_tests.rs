use super::*;

#[test]
fn default_profile_matches_qwen2_vl() {
    let p = ImageTokenProfile::default();
    // 28px stride: 448×448 → 16×16 = 256 tokens.
    assert_eq!(p.tokens_for(448, 448), 256);
    // Non-square: 1264×1412 → 45×51 = 2295 → clamped to the max.
    assert_eq!(p.tokens_for(1264, 1412), 1280);
    // A very large image is clamped to the upper bound.
    assert_eq!(p.tokens_for(4000, 4000), 1280);
    // A tiny image is clamped up to the lower bound.
    assert_eq!(p.tokens_for(1, 1), 4);
}

#[test]
fn estimate_prefers_known_then_sniffed_then_unknown() {
    let p = ImageTokenProfile::default();
    assert_eq!(estimate_image_tokens(Some((448, 448)), Some((28, 28)), &p), 256);
    // Zero/absent known dims fall through to the sniffed value.
    assert_eq!(estimate_image_tokens(Some((0, 0)), Some((448, 448)), &p), 256);
    assert_eq!(estimate_image_tokens(None, Some((56, 56)), &p), 4);
    assert_eq!(estimate_image_tokens(None, None, &p), 512);
}

#[test]
fn parses_png_dimensions() {
    let mut b = vec![0u8; 33];
    b[..8].copy_from_slice(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);
    b[16..20].copy_from_slice(&100u32.to_be_bytes());
    b[20..24].copy_from_slice(&50u32.to_be_bytes());
    assert_eq!(parse_image_dimensions(&b), Some((100, 50)));
}

#[test]
fn parses_gif_dimensions() {
    let mut b = vec![0u8; 13];
    b[..4].copy_from_slice(b"GIF8");
    b[6..8].copy_from_slice(&320u16.to_le_bytes());
    b[8..10].copy_from_slice(&240u16.to_le_bytes());
    assert_eq!(parse_image_dimensions(&b), Some((320, 240)));
}

#[test]
fn parses_webp_extended_dimensions() {
    let mut b = vec![0u8; 30];
    b[..4].copy_from_slice(b"RIFF");
    b[8..12].copy_from_slice(b"WEBP");
    b[12..16].copy_from_slice(b"VP8X");
    b[24..27].copy_from_slice(&299u32.to_le_bytes()[..3]);
    b[27..30].copy_from_slice(&199u32.to_le_bytes()[..3]);
    assert_eq!(parse_image_dimensions(&b), Some((300, 200)));
}

#[test]
fn parses_jpeg_sof_dimensions() {
    let mut b = vec![0u8; 12];
    b[0] = 0xFF;
    b[1] = 0xD8; // SOI
    b[2] = 0xFF;
    b[3] = 0xC0; // SOF0
    b[4..6].copy_from_slice(&17u16.to_be_bytes()); // segment length
    b[6] = 8; // precision
    b[7..9].copy_from_slice(&480u16.to_be_bytes()); // height
    b[9..11].copy_from_slice(&640u16.to_be_bytes()); // width
    assert_eq!(parse_image_dimensions(&b), Some((640, 480)));
}

#[test]
fn parse_rejects_unknown_or_truncated() {
    assert_eq!(parse_image_dimensions(&[]), None);
    assert_eq!(parse_image_dimensions(&[1, 2, 3, 4, 5]), None);
    // Truncated PNG header.
    let mut b = vec![0u8; 8];
    b.copy_from_slice(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);
    assert_eq!(parse_image_dimensions(&b), None);
}

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
fn clear_drops_the_anchor() {
    let mut stats = TokenStats::new();
    stats.set_context_anchor(50_000, 100_000);
    stats.clear_context_anchor();
    assert_eq!(stats.anchored_context_estimate(38_000), 10_000);
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

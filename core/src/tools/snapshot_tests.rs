use super::*;

fn store_with(path: &str, text: &str) -> (SnapshotStore, String) {
    let mut store = SnapshotStore::default();
    let tag = store.record(path, text, None);
    (store, tag)
}

#[test]
fn tag_is_4_hex_uppercase_and_stable() {
    let (_, tag) = store_with("a.txt", "hello\n");
    assert_eq!(tag.len(), 4);
    assert!(
        tag.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    );
    let (_, tag2) = store_with("a.txt", "hello\n");
    assert_eq!(tag, tag2);
}

#[test]
fn different_content_yields_different_tag() {
    let (_, a) = store_with("a.txt", "hello\n");
    let (_, b) = store_with("a.txt", "world\n");
    assert_ne!(a, b);
}

#[test]
fn head_returns_latest_snapshot() {
    let (mut store, tag) = store_with("a.txt", "alpha\n");
    assert_eq!(store.head("a.txt").unwrap().tag, tag);
    assert!(store.head("missing.txt").is_none());

    store.record("a.txt", "beta\n", None);
    assert_eq!(store.head("a.txt").unwrap().text, "beta\n");
}

#[test]
fn repeated_read_merges_seen_lines() {
    let mut store = SnapshotStore::default();
    store.record("a.txt", "value\n", Some(&[1]));
    store.record("a.txt", "value\n", Some(&[2]));
    assert_eq!(
        store
            .head("a.txt")
            .unwrap()
            .seen_lines
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
}

#[test]
fn normalize_strips_bom_and_crlf() {
    assert_eq!(normalize_text("\u{feff}a\r\nb\rc"), "a\nb\nc");
    assert_eq!(normalize_text("plain\n"), "plain\n");
}

#[test]
fn compute_tag_matches_across_line_endings() {
    // CRLF and LF of the same logical content must hash identically.
    assert_eq!(compute_tag(&normalize_text("a\r\nb")), compute_tag("a\nb"));
}

#[test]
fn snapshot_keeps_full_digest_separate_from_display_tag() {
    let mut store = SnapshotStore::with_dedup(true);
    let first = "version = 18\n";
    let second = "version = 93\n";
    assert_eq!(compute_tag(first), compute_tag(second));
    assert_ne!(compute_digest(first), compute_digest(second));

    store.record("a.txt", first, Some(&[1]));
    assert_eq!(store.head("a.txt").unwrap().digest, compute_digest(first));
    assert!(!store.take_unchanged("a.txt", &compute_digest(first), 1, 1));
    assert!(!store.take_unchanged("a.txt", &compute_digest(second), 1, 1));
}

#[test]
fn unchanged_read_is_consumed_and_mismatches_rearm() {
    let digest = compute_digest("content");
    let mut store = SnapshotStore::with_dedup(true);
    assert!(!store.take_unchanged("a.txt", &digest, 1, 2));
    assert!(store.take_unchanged("a.txt", &digest, 1, 2));
    assert!(!store.take_unchanged("a.txt", &digest, 1, 2));

    assert!(!store.take_unchanged("a.txt", &digest, 3, 4));
    assert!(store.take_unchanged("a.txt", &digest, 3, 4));
    assert!(!store.take_unchanged("a.txt", &compute_digest("changed"), 3, 4));
    assert!(store.take_unchanged("a.txt", &compute_digest("changed"), 3, 4));
}

#[test]
fn clearing_store_and_disabled_dedup_drop_pending_reads() {
    let digest = compute_digest("content");
    let mut store = SnapshotStore::with_dedup(true);
    assert!(!store.take_unchanged("a.txt", &digest, 1, 1));
    store.clear();
    assert!(!store.take_unchanged("a.txt", &digest, 1, 1));

    let mut disabled = SnapshotStore::with_dedup(false);
    assert!(!disabled.take_unchanged("a.txt", &digest, 1, 1));
    assert!(!disabled.take_unchanged("a.txt", &digest, 1, 1));
}

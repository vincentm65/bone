use super::*;

fn store_with(path: &str, text: &str) -> (SnapshotStore, String) {
    let mut store = SnapshotStore::new();
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
    let mut store = SnapshotStore::new();
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
fn clear_removes_snapshots() {
    let (mut store, _) = store_with("a.txt", "x\n");
    store.clear();
    assert!(store.head("a.txt").is_none());
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

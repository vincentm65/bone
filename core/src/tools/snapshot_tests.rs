use super::*;

fn store_with(path: &str, text: &str) -> (SnapshotStore, String) {
    let mut s = SnapshotStore::new();
    let tag = s.record(path, text, None);
    (s, tag)
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
fn head_by_hash_by_content() {
    let (s, tag) = store_with("a.txt", "alpha\nbeta\n");
    assert_eq!(s.head("a.txt").unwrap().tag, tag);
    assert_eq!(s.by_hash("a.txt", &tag).unwrap().text, "alpha\nbeta\n");
    assert_eq!(s.by_content("a.txt", "alpha\nbeta\n").unwrap().tag, tag);
    assert!(s.by_hash("a.txt", "0000").is_none());
    assert!(s.head("missing.txt").is_none());
}

#[test]
fn record_dedupes_and_promotes_identical_content() {
    let mut s = SnapshotStore::new();
    let _ = s.record("a.txt", "v1\n", Some(&[1]));
    let t2 = s.record("a.txt", "v2\n", None);
    // Re-read v1 with an additional seen line: promotes v1 to head, merges.
    let _ = s.record("a.txt", "v1\n", Some(&[1, 2]));
    assert_eq!(s.version_count("a.txt"), 2, "no duplicate");
    let head = s.head("a.txt").unwrap();
    assert_eq!(head.text, "v1\n");
    assert!(head.saw_line(1) && head.saw_line(2));
    // v2 still recoverable by tag.
    assert!(s.by_hash("a.txt", &t2).is_some());
}

#[test]
fn record_seen_lines_merges() {
    let (mut s, tag) = store_with("a.txt", "x\n");
    assert!(s.head("a.txt").unwrap().seen_lines.is_empty());
    s.record_seen_lines("a.txt", &tag, &[3, 4]);
    let snap = s.by_hash("a.txt", &tag).unwrap();
    assert_eq!(
        snap.seen_lines.iter().copied().collect::<Vec<_>>(),
        vec![3, 4]
    );
}

#[test]
fn max_versions_eviction() {
    let mut s = SnapshotStore::new();
    for i in 0..(MAX_VERSIONS + 3) {
        s.record("a.txt", &format!("v{i}\n"), None);
    }
    assert_eq!(s.version_count("a.txt"), MAX_VERSIONS);
    // Newest retained; oldest evicted.
    assert_eq!(s.head("a.txt").unwrap().text, "v6\n");
    assert!(s.by_content("a.txt", "v0\n").is_none());
}

#[test]
fn invalidate_relocate_clear() {
    let (mut s, _) = store_with("a.txt", "x\n");
    s.relocate("a.txt", "b.txt");
    assert!(s.head("a.txt").is_none());
    assert!(s.head("b.txt").is_some());
    s.invalidate("b.txt");
    assert!(s.head("b.txt").is_none());
    let _ = s.record("c.txt", "y\n", None);
    s.clear();
    assert!(s.head("c.txt").is_none());
}

#[test]
fn relocate_same_path_is_noop() {
    let (mut s, tag) = store_with("a.txt", "x\n");
    s.relocate("a.txt", "a.txt");
    assert_eq!(s.head("a.txt").unwrap().tag, tag);
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

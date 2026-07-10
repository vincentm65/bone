//! Session-scoped snapshot store for hashline edits.
//!
//! Records the normalized content of each file `read_file`/`write_file`
//! produced, keyed by a short content tag. `edit_file` references the tag to
//! prove it is editing a known, current snapshot, and to recover from stale
//! reads. Lives behind an `Arc<RwLock<..>>` on the [`crate::tools::registry::ToolHandler`]
//! so it is shared across tool calls within a session and persists across turns
//! (the `ToolHandler` is cloned per turn but the `Arc` is shared, and the
//! driver never reassigns it — see `runtime/session.rs`).
//!
//! Model (OMP "hashline"): per path we keep the most recent read plus a small
//! ring of prior versions; each carries the line numbers the model actually
//! saw, so the visible-line guard can reject edits to elided lines.

use std::collections::{BTreeSet, HashMap, VecDeque};

use sha2::{Digest, Sha256};

/// Upper bound on retained versions per path (for stale recovery). Mirrors
/// OMP's default of 4.
const MAX_VERSIONS: usize = 4;

/// One recorded file snapshot.
#[derive(Clone, Debug)]
pub struct Snapshot {
    /// Normalized full file text (LF line endings, BOM stripped).
    pub text: String,
    /// 4-hex content tag (uppercase), derived from `text` via [`compute_tag`].
    pub tag: String,
    /// Lines (1-indexed) the model actually saw in the read output. Edits may
    /// only anchor on these; the visible-line guard rejects the rest.
    pub seen_lines: BTreeSet<usize>,
}

impl Snapshot {
    /// True if `line` (1-indexed) was shown to the model in this snapshot.
    pub fn saw_line(&self, line: usize) -> bool {
        self.seen_lines.contains(&line)
    }
}

#[derive(Debug, Default)]
struct History {
    /// Newest version at the back; capped at [`MAX_VERSIONS`].
    versions: VecDeque<Snapshot>,
}

/// Per-session store of file snapshots, keyed by path.
#[derive(Debug, Default)]
pub struct SnapshotStore {
    paths: HashMap<String, History>,
}

impl SnapshotStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Most recent snapshot for `path`, if any.
    pub fn head(&self, path: &str) -> Option<&Snapshot> {
        self.paths.get(path).and_then(|h| h.versions.back())
    }

    /// Any retained version of `path` whose tag matches `tag`.
    pub fn by_hash(&self, path: &str, tag: &str) -> Option<&Snapshot> {
        self.paths.get(path)?.versions.iter().find(|s| s.tag == tag)
    }

    /// Any retained version of `path` whose normalized text equals `text`.
    pub fn by_content(&self, path: &str, text: &str) -> Option<&Snapshot> {
        self.paths
            .get(path)?
            .versions
            .iter()
            .find(|s| s.text == text)
    }

    /// Record a normalized snapshot for `path`, returning its tag. If an
    /// identical version already exists it is promoted to newest (and its
    /// seen-lines merged) so a re-read neither duplicates nor loses visibility.
    pub fn record(&mut self, path: &str, text: &str, seen_lines: Option<&[usize]>) -> String {
        let tag = compute_tag(text);
        let history = self.paths.entry(path.to_string()).or_default();

        if let Some(pos) = history.versions.iter().position(|s| s.text == text) {
            let mut snap = history.versions.remove(pos).expect("position is valid");
            if let Some(lines) = seen_lines {
                snap.seen_lines.extend(lines.iter().copied());
            }
            history.versions.push_back(snap);
            return tag;
        }

        let mut seen = BTreeSet::new();
        if let Some(lines) = seen_lines {
            seen.extend(lines.iter().copied());
        }
        history.versions.push_back(Snapshot {
            text: text.to_string(),
            tag: tag.clone(),
            seen_lines: seen,
        });
        while history.versions.len() > MAX_VERSIONS {
            history.versions.pop_front();
        }
        tag
    }

    /// Merge additional seen-lines into the version tagged `tag` for `path`.
    pub fn record_seen_lines(&mut self, path: &str, tag: &str, lines: &[usize]) {
        let Some(history) = self.paths.get_mut(path) else {
            return;
        };
        if let Some(snap) = history.versions.iter_mut().find(|s| s.tag == tag) {
            snap.seen_lines.extend(lines.iter().copied());
        }
    }

    /// Drop all versions for `path` (e.g. after the file is deleted).
    pub fn invalidate(&mut self, path: &str) {
        self.paths.remove(path);
    }

    /// Move all versions from `from` to `to` (rename).
    pub fn relocate(&mut self, from: &str, to: &str) {
        if from == to {
            return;
        }
        if let Some(history) = self.paths.remove(from) {
            self.paths.insert(to.to_string(), history);
        }
    }

    /// Clear everything (session reset).
    pub fn clear(&mut self) {
        self.paths.clear();
    }

    /// Number of retained versions for `path` (diagnostic/test helper).
    pub fn version_count(&self, path: &str) -> usize {
        self.paths.get(path).map(|h| h.versions.len()).unwrap_or(0)
    }
}

/// Normalize file text for hashing/snapshotting: strip a leading BOM and
/// convert CRLF / lone CR to LF. Only line-ending normalization is applied so
/// the tag reflects exactly the bytes `read_file` and `edit_file` both see.
pub fn normalize_text(text: &str) -> String {
    let without_bom = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut out = String::with_capacity(without_bom.len());
    let mut chars = without_bom.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' {
            out.push('\n');
            // Swallow the paired LF of a CRLF so we don't double the newline.
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Split normalized text into 1-indexed lines (no trailing empty element for a
/// terminal newline). Shared by `read_file` (to number displayed lines) and the
/// apply engine (to index snapshot lines).
pub fn numbered_lines(text: &str) -> Vec<&str> {
    text.lines().collect()
}

/// 4-hex uppercase content tag for normalized `text`.
pub fn compute_tag(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for b in digest.iter() {
        hex.push_str(&format!("{b:02x}"));
    }
    hex[..4].to_uppercase()
}

#[cfg(test)]
mod tests {
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
}

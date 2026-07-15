//! Session-scoped snapshot store for safe file edits.
//!
//! Records the normalized content of each file `read_file`/`write_file`
//! produced. `edit_file` uses the latest snapshot internally to prove it is
//! editing text the model saw and to detect stale reads. Lives behind an
//! `Arc<RwLock<..>>` on the [`crate::tools::registry::ToolHandler`]
//! so it is shared across tool calls within a session and persists across turns
//! (the `ToolHandler` is cloned per turn but the `Arc` is shared, and the
//! driver never reassigns it — see `runtime/session.rs`).
//!
//! Per path we keep the most recent read plus a small ring of prior versions;
//! each carries the line numbers the model actually saw, so the visibility
//! guard can reject edits to elided lines.

use std::collections::{BTreeSet, HashMap, VecDeque};

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::fs;

/// Anchor a path to the session working directory. Absolute paths are unchanged.
pub fn resolve_path(path: &str, working_dir: Option<&Path>) -> Result<PathBuf, String> {
    if path.trim().is_empty() {
        return Err("`path` must not be empty".to_string());
    }
    let path = PathBuf::from(path);
    Ok(if path.is_relative() {
        working_dir.map_or(path.clone(), |cwd| cwd.join(path))
    } else {
        path
    })
}

/// Resolve an existing path to one stable identity. Canonicalization collapses
/// `.`/`..` and makes equivalent symlinked paths share snapshots.
pub async fn resolve_existing_path(
    path: &str,
    working_dir: Option<&Path>,
) -> Result<PathBuf, String> {
    let target = resolve_path(path, working_dir)?;
    fs::canonicalize(target)
        .await
        .map_err(|e| format!("could not resolve `{path}`: {e}"))
}

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
#[path = "snapshot_tests.rs"]
mod snapshot_tests;

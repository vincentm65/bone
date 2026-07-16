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
//! Per path we keep the most recent read and the line numbers the model
//! actually saw, so the visibility guard can reject edits to elided lines.

use std::collections::{BTreeSet, HashMap};

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

/// Resolve a not-yet-created path without allowing it to escape `working_dir`.
/// Canonicalizing the nearest existing ancestor handles both `..` and symlinked
/// parent directories while still permitting missing intermediate directories.
pub async fn resolve_new_path(path: &str, working_dir: Option<&Path>) -> Result<PathBuf, String> {
    let target = resolve_path(path, working_dir)?;
    let Some(working_dir) = working_dir else {
        return Ok(target);
    };
    let boundary = fs::canonicalize(working_dir)
        .await
        .map_err(|e| format!("could not resolve working directory: {e}"))?;

    let mut ancestor = target.as_path();
    loop {
        match fs::symlink_metadata(ancestor).await {
            Ok(_) => break,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                ancestor = ancestor
                    .parent()
                    .ok_or_else(|| format!("could not resolve `{path}`"))?;
            }
            Err(e) => return Err(format!("could not resolve `{path}`: {e}")),
        }
    }
    let mut resolved = fs::canonicalize(ancestor)
        .await
        .map_err(|e| format!("could not resolve `{path}`: {e}"))?;
    for component in target
        .strip_prefix(ancestor)
        .map_err(crate::util::errstr)?
        .components()
    {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                resolved.pop();
            }
            std::path::Component::Normal(component) => resolved.push(component),
            std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                return Err(format!("could not resolve `{path}`"));
            }
        }
    }
    if !resolved.starts_with(&boundary) {
        return Err(format!(
            "path `{path}` is outside the working directory `{}`",
            working_dir.display()
        ));
    }
    Ok(resolved)
}

/// Most recently recorded state of a file.
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

/// Per-session store of the latest file snapshot, keyed by path.
#[derive(Debug, Default)]
pub struct SnapshotStore {
    paths: HashMap<String, Snapshot>,
}

impl SnapshotStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Most recent snapshot for `path`, if any.
    pub fn head(&self, path: &str) -> Option<&Snapshot> {
        self.paths.get(path)
    }

    /// Record a normalized snapshot for `path`, returning its tag. A repeated
    /// read of identical content merges the lines visible to the model.
    pub fn record(&mut self, path: &str, text: &str, seen_lines: Option<&[usize]>) -> String {
        let tag = compute_tag(text);
        if let Some(snapshot) = self.paths.get_mut(path)
            && snapshot.text == text
        {
            if let Some(lines) = seen_lines {
                snapshot.seen_lines.extend(lines.iter().copied());
            }
            return tag;
        }

        let mut seen = BTreeSet::new();
        if let Some(lines) = seen_lines {
            seen.extend(lines.iter().copied());
        }
        self.paths.insert(
            path.to_string(),
            Snapshot {
                text: text.to_string(),
                tag: tag.clone(),
                seen_lines: seen,
            },
        );
        tag
    }

    /// Clear everything (session reset).
    pub fn clear(&mut self) {
        self.paths.clear();
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

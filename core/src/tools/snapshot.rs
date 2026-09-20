//! Session-scoped snapshot store for safe file edits.
//!
//! Records the normalized content of each file `read_file`/`create_file`
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
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, RwLock};

use sha2::{Digest, Sha256};
use tokio::fs;

pub type Snapshots = Arc<RwLock<SnapshotStore>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    CrLf,
    Cr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextFormat {
    pub line_ending: LineEnding,
    pub has_bom: bool,
}

impl TextFormat {
    pub fn detect(text: &str) -> Self {
        let bytes = text.as_bytes();
        let line_ending = bytes
            .iter()
            .position(|byte| *byte == b'\r' || *byte == b'\n')
            .map_or(LineEnding::Lf, |index| {
                if bytes[index] == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
                    LineEnding::CrLf
                } else if bytes[index] == b'\r' {
                    LineEnding::Cr
                } else {
                    LineEnding::Lf
                }
            });
        Self {
            line_ending,
            has_bom: text.starts_with('\u{feff}'),
        }
    }

    pub fn restore_newlines(self, normalized: &str) -> String {
        let newline = match self.line_ending {
            LineEnding::Lf => "\n",
            LineEnding::CrLf => "\r\n",
            LineEnding::Cr => "\r",
        };
        normalized.replace('\n', newline)
    }
}

/// Component that names the Bone config directory in prompt-facing paths.
const CONFIG_DIR_PREFIX: &str = ".bone-rust";

/// Anchor a path to the session working directory. Absolute paths are unchanged.
///
/// A leading `.bone-rust` component means the resolved config directory — the
/// shape Bone's system prompt and `AGENTS.md` use — so it is anchored there
/// instead of the working directory. A working directory that really contains
/// such an entry keeps winning, so project-local trees stay reachable.
pub fn resolve_path(path: &str, working_dir: Option<&Path>) -> Result<PathBuf, String> {
    if path.trim().is_empty() {
        return Err("`path` must not be empty".to_string());
    }
    let path = PathBuf::from(path);
    if path.is_absolute() {
        return Ok(path);
    }
    let cwd = working_dir
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok());
    if let Some(rest) = config_relative(&path) {
        let shadowed = cwd
            .as_deref()
            .is_some_and(|cwd| cwd.join(CONFIG_DIR_PREFIX).exists());
        if !shadowed && let Some(bone) = crate::config::try_bone_dir() {
            return Ok(bone.join(rest));
        }
    }
    Ok(match cwd {
        Some(cwd) => cwd.join(path),
        None => path,
    })
}

/// Split the leading `.bone-rust` component off `path`, if it has one.
fn config_relative(path: &Path) -> Option<PathBuf> {
    let trimmed = path.strip_prefix(".").unwrap_or(path);
    let trimmed = if trimmed.as_os_str().is_empty() {
        path
    } else {
        trimmed
    };
    trimmed
        .strip_prefix(CONFIG_DIR_PREFIX)
        .ok()
        .map(Path::to_path_buf)
}

/// Resolve an existing path to one stable identity. Canonicalization collapses
/// `.`/`..` and makes equivalent symlinked paths share snapshots. Missing paths
/// get a bounded filename repair/suggestion pass; other filesystem errors are
/// returned unchanged.
pub async fn resolve_existing_path(
    path: &str,
    working_dir: Option<&Path>,
) -> Result<PathBuf, String> {
    let target = resolve_path(path, working_dir)?;
    reject_stream_path(&target)?;

    match fs::canonicalize(&target).await {
        Ok(resolved) => {
            reject_stream_path(&resolved)?;
            Ok(resolved)
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {
            let Some(name) = target.file_name().and_then(|name| name.to_str()) else {
                return Err(format!("could not resolve `{path}`: {error}"));
            };
            let parent = target.parent().unwrap_or_else(|| Path::new("."));
            let boundary = if path_is_relative(path) {
                match working_dir {
                    Some(cwd) => fs::canonicalize(cwd).await.ok(),
                    None => None,
                }
            } else {
                None
            };
            let mut repaired = Vec::new();
            for variant in crate::tools::path_repair::variants(name) {
                let candidate = parent.join(&variant);
                let Ok(resolved) = fs::canonicalize(&candidate).await else {
                    continue;
                };
                if boundary
                    .as_deref()
                    .is_some_and(|cwd| !resolved.starts_with(cwd))
                {
                    continue;
                }
                repaired.push(resolved);
            }
            if repaired.len() == 1 {
                let resolved = repaired.pop().expect("one repaired path");
                reject_stream_path(&resolved)?;
                return Ok(resolved);
            }

            let suggestions = crate::tools::path_repair::suggest(parent, name).await;
            let hint = if suggestions.is_empty() {
                String::new()
            } else {
                format!(" Did you mean: {}?", suggestions.join(", "))
            };
            Err(format!("could not resolve `{path}`: {error}.{hint}"))
        }
        Err(error) => Err(format!("could not resolve `{path}`: {error}")),
    }
}

fn path_is_relative(path: &str) -> bool {
    Path::new(path).is_relative()
}

/// Refuse names that can resolve to a live stream even when their target is
/// reported as a regular file (for example `/dev/stdin` with redirected input).
fn reject_stream_path(path: &Path) -> Result<(), String> {
    let protected_name = matches!(
        path.to_str(),
        Some("/dev/stdin") | Some("/dev/stdout") | Some("/dev/stderr")
    );
    if protected_name || is_proc_fd_path(path) {
        return Err(format!(
            "`{}` is a protected device path; refusing to read it — use shell if you really mean to stream from it",
            path.display()
        ));
    }
    Ok(())
}

fn is_proc_fd_path(path: &Path) -> bool {
    let mut components = path.components();
    matches!(
        (
            components.next(),
            components.next(),
            components.next(),
            components.next(),
            components.next(),
            components.next()
        ),
        (
            Some(Component::RootDir),
            Some(Component::Normal(proc)),
            Some(Component::Normal(pid)),
            Some(Component::Normal(fd)),
            Some(Component::Normal(number)),
            None
        ) if proc == "proc" && fd == "fd" && (pid == "self" || pid.to_str().is_some_and(|pid| pid.bytes().all(|b| b.is_ascii_digit()))) && number.to_str().is_some_and(|number| number.bytes().all(|b| b.is_ascii_digit()))
    )
}

/// Stat once: reject anything that is not a regular file, then enforce the byte
/// cap. The type check comes first because special files can report size zero.
pub async fn ensure_readable_regular_file(path: &str, max_bytes: u64) -> Result<(), String> {
    reject_stream_path(Path::new(path))?;
    let metadata = fs::metadata(path).await.map_err(crate::util::errstr)?;
    if !metadata.is_file() {
        return Err(format!(
            "`{path}` is a {}, not a regular file; refusing to read it — use shell if you really mean to stream from it",
            file_kind(&metadata)
        ));
    }
    if metadata.len() > max_bytes {
        return Err(format!(
            "file is {:.1} MB; too large to read directly — use shell (head/tail/rg)",
            metadata.len() as f64 / (1024.0 * 1024.0)
        ));
    }
    Ok(())
}

fn file_kind(metadata: &std::fs::Metadata) -> &'static str {
    let file_type = metadata.file_type();
    if file_type.is_dir() {
        return "a directory";
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        if file_type.is_char_device() {
            return "a character device";
        }
        if file_type.is_block_device() {
            return "a block device";
        }
        if file_type.is_fifo() {
            return "a FIFO";
        }
        if file_type.is_socket() {
            return "a socket";
        }
    }
    "a special file"
}

/// Most recently recorded state of a file.
#[derive(Clone, Debug)]
pub struct Snapshot {
    /// Normalized full file text (LF line endings, BOM stripped).
    pub text: String,
    /// Original BOM and line-ending convention.
    pub format: TextFormat,
    /// Full SHA-256 identity of the normalized text.
    pub digest: [u8; 32],
    /// 4-hex content tag (uppercase), derived from `text` via [`compute_tag`].
    pub tag: String,
    /// Lines (1-indexed) the model actually saw in the read output. Edits may
    /// only anchor on these; the visible-line guard rejects the rest.
    pub seen_lines: BTreeSet<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LastRead {
    digest: [u8; 32],
    first: usize,
    end: usize,
}

/// Per-session store of the latest file snapshot, keyed by path.
#[derive(Debug)]
pub struct SnapshotStore {
    paths: HashMap<String, Snapshot>,
    last_reads: HashMap<String, LastRead>,
    dedup_enabled: bool,
}

impl Default for SnapshotStore {
    fn default() -> Self {
        let enabled = std::env::var("BONE_READ_DEDUP").as_deref() != Ok("0");
        Self::with_dedup(enabled)
    }
}

impl SnapshotStore {
    /// Construct a store with unchanged-read deduplication explicitly enabled
    /// or disabled. The default reads `BONE_READ_DEDUP` once at construction.
    pub fn with_dedup(enabled: bool) -> Self {
        Self {
            paths: HashMap::new(),
            last_reads: HashMap::new(),
            dedup_enabled: enabled,
        }
    }

    /// Most recent snapshot for `path`, if any.
    pub fn head(&self, path: &str) -> Option<&Snapshot> {
        self.paths.get(path)
    }

    /// Record a normalized snapshot for `path`, returning its tag. A repeated
    /// read of identical content merges the lines visible to the model.
    pub fn record(&mut self, path: &str, text: &str, seen_lines: Option<&[usize]>) -> String {
        self.record_with_format(path, text, TextFormat::detect(text), seen_lines)
    }

    pub fn record_with_format(
        &mut self,
        path: &str,
        text: &str,
        format: TextFormat,
        seen_lines: Option<&[usize]>,
    ) -> String {
        let digest = compute_digest(text);
        let tag = compute_tag_from_digest(&digest);
        if let Some(snapshot) = self.paths.get_mut(path)
            && snapshot.text == text
            && snapshot.format == format
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
                format,
                digest,
                tag: tag.clone(),
                seen_lines: seen,
            },
        );
        tag
    }

    /// Consume a matching previous read, or arm this read for the next call.
    /// A mismatch replaces stale state, so a changed file/window is real once
    /// and its unchanged repeat is the next deduplicated call.
    pub fn take_unchanged(
        &mut self,
        path: &str,
        digest: &[u8; 32],
        first: usize,
        end: usize,
    ) -> bool {
        if !self.dedup_enabled {
            return false;
        }
        let current = LastRead {
            digest: *digest,
            first,
            end,
        };
        if self.last_reads.get(path) == Some(&current) {
            self.last_reads.remove(path);
            true
        } else {
            self.last_reads.insert(path.to_string(), current);
            false
        }
    }

    /// Do not arm unchanged-read deduplication for a response with no visible
    /// lines, such as an empty or out-of-range file.
    pub fn clear_last_read(&mut self, path: &str) {
        self.last_reads.remove(path);
    }

    /// Clear everything (session reset).
    pub fn clear(&mut self) {
        self.paths.clear();
        self.last_reads.clear();
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

/// Full SHA-256 identity for normalized `text`.
pub fn compute_digest(text: &str) -> [u8; 32] {
    Sha256::digest(text.as_bytes()).into()
}

fn compute_tag_from_digest(digest: &[u8; 32]) -> String {
    let mut hex = String::with_capacity(digest.len() * 2);
    for b in digest {
        hex.push_str(&format!("{b:02x}"));
    }
    hex[..4].to_uppercase()
}

/// 4-hex uppercase content tag for normalized `text`.
pub fn compute_tag(text: &str) -> String {
    compute_tag_from_digest(&compute_digest(text))
}

#[cfg(test)]
#[path = "snapshot_tests.rs"]
mod snapshot_tests;

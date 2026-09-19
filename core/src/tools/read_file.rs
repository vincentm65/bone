//! The `read_file` tool: text and image reading with optional line ranges.
//!
//! Text is returned with a simple file/range header and numbered lines. The
//! full content and visible lines are recorded internally so `edit_file` can
//! validate an exact replacement without making the model repeat hashes or a
//! custom patch language. Image files are returned as attachments unchanged.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use base64::Engine;
use globset::{GlobBuilder, GlobMatcher};
use ignore::WalkBuilder;
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::ErrorKind;
use tokio::fs;

use crate::llm::ImageData;
use crate::tools::snapshot::{self, Snapshots};
use crate::tools::types::{Tool, ToolDefinition, ToolExecutionContext, ToolOutput};
use crate::tools::{MAX_TOOL_LINE_CHARS, truncate_line};

pub struct ReadFileTool;

/// Map a file extension to an image MIME type, if it is a supported image.
fn image_media_type(path: &str) -> Option<&'static str> {
    let ext = path.rsplit('.').next()?.to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

const MAX_TEXT_FILE_BYTES: u64 = 50 * 1024 * 1024;
const MAX_IMAGE_FILE_BYTES: u64 = 10 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 50 * 1024;
const MAX_BULK_FILES: usize = 50;
const MAX_BULK_OUTPUT_BYTES: usize = 100 * 1024;
/// Default window when the model omits `max_lines`. High enough that typical
/// source files fit in one full read (safer first-try edits) while still
/// hard-capped at the schema maximum of 1000.
const DEFAULT_MAX_LINES: usize = 1000;

fn ensure_len(len: u64, max_bytes: u64) -> Result<(), String> {
    if len > max_bytes {
        return Err(format!(
            "file is {:.1} MB; too large to read directly — use shell (head/tail/rg)",
            len as f64 / (1024.0 * 1024.0)
        ));
    }
    Ok(())
}

/// Plural suffix: "" for 1, "s" otherwise.
fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn range_header(
    path: &str,
    first: usize,
    end: usize,
    total: usize,
    stopped: Option<&str>,
    truncated_count: usize,
) -> String {
    let mut header = format!("File: {path}\n");
    if end < total {
        header.push_str(&format!("Range: lines {first}-{end} of {total}.\n"));
        if let Some(reason) = stopped {
            header.push_str(&format!("Stopped: {reason}.\n"));
        }
        header.push_str(&format!(
            "Continue: read_file(path={path:?}, start_line={}).\n",
            end + 1
        ));
    } else if first > 1 {
        header.push_str(&format!(
            "Range: lines {first}-{end} of {total}; end of file.\n"
        ));
    } else {
        header.push_str(&format!("Range: lines 1-{end} of {total}; entire file.\n"));
    }
    if truncated_count > 0 {
        header.push_str(&format!(
            "Inspect: {truncated_count} overlong line{} truncated and not editable; use shell.\n",
            plural(truncated_count),
        ));
    }
    header
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StringList {
    One(String),
    Many(Vec<String>),
}

fn deserialize_string_list<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(match StringList::deserialize(deserializer)? {
        StringList::One(value) => vec![value],
        StringList::Many(values) => values,
    })
}

#[derive(Deserialize)]
struct Args {
    path: String,
    #[serde(default, deserialize_with = "deserialize_string_list")]
    paths: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_string_list")]
    exclude: Vec<String>,
    start_line: Option<usize>,
    max_lines: Option<usize>,
}

impl Args {
    fn is_bulk(&self) -> bool {
        !self.paths.is_empty()
            || !self.exclude.is_empty()
            || has_glob_magic(&self.path)
            || self.paths.iter().any(|path| has_glob_magic(path))
    }

    fn literal(path: String) -> Self {
        Self {
            path,
            paths: Vec::new(),
            exclude: Vec::new(),
            start_line: None,
            max_lines: None,
        }
    }
}

fn has_glob_magic(path: &str) -> bool {
    path.chars()
        .any(|character| matches!(character, '*' | '?' | '[' | '{'))
}

#[async_trait]
impl Tool for ReadFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read_file".to_string(),
            description:
                "Preferred tool for reading file contents; use this instead of shell commands such as cat, head, tail, or sed. Reads a UTF-8 text file and returns the resolved path, range information, and numbered lines, stopping at 50 KiB of output. To edit, copy an exact unique block of shown text into edit_file.old_text and provide its replacement as new_text. Optionally pass start_line and max_lines; defaults to the first 1000 lines. `path` may be a glob (`*` and `?` never cross directory boundaries; use `**` to recurse); use additive `paths` for more literals or globs and `exclude` to omit matches. Bulk reads skip hidden files and honor .gitignore, cap the match count and aggregate output, and preserve image attachments. Image files (png, jpg, jpeg, gif, webp) are returned as an image you can view."
                    .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path or glob to read. Relative paths resolve from the working directory."
                    },
                    "paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Additional literal paths or globs to read."
                    },
                    "exclude": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Glob patterns to omit from a bulk read."
                    },
                    "start_line": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "1-based first line to include. Omit to start at line 1."
                    },
                    "max_lines": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 1000,
                        "description": "Max lines to return. Defaults to 1000."
                    }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(&self, arguments: Value) -> Result<String, String> {
        self.read_file_inner(arguments, None, None)
            .await
            .map(|output| output.content)
    }

    async fn execute_output(&self, arguments: Value) -> Result<ToolOutput, String> {
        self.read_file_inner(arguments, None, None).await
    }

    async fn execute_output_live(
        &self,
        arguments: Value,
        _events: Option<tokio::sync::mpsc::UnboundedSender<crate::pane_content::KeyRequest>>,
        context: ToolExecutionContext,
    ) -> Result<ToolOutput, String> {
        self.read_file_inner(
            arguments,
            Some(&context.snapshots),
            context.working_dir.as_deref(),
        )
        .await
    }
}

impl ReadFileTool {
    /// Read a file. Image files become attachments; text files take the
    /// text path (recording the internal snapshot when a store is provided).
    async fn read_file_inner(
        &self,
        arguments: Value,
        snapshots: Option<&Snapshots>,
        working_dir: Option<&Path>,
    ) -> Result<ToolOutput, String> {
        let args: Args = serde_json::from_value(arguments).map_err(crate::util::errstr)?;
        if args.is_bulk() {
            return read_bulk(&args, snapshots, working_dir).await;
        }

        let path = &args.path;
        if let Some(media_type) = image_media_type(path) {
            let resolved = snapshot::resolve_existing_path(path, working_dir).await?;
            return read_image_resolved(&resolved, media_type).await;
        }

        let text = read_text(&args, snapshots, working_dir).await?;
        Ok(ToolOutput::text(text))
    }
}

async fn read_image_resolved(resolved: &Path, media_type: &str) -> Result<ToolOutput, String> {
    let path = resolved.to_string_lossy().into_owned();
    ensure_readable(&path, MAX_IMAGE_FILE_BYTES).await?;
    let bytes = fs::read(resolved).await.map_err(crate::util::errstr)?;
    ensure_len(bytes.len() as u64, MAX_IMAGE_FILE_BYTES)?;
    let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let note = format!("[read image {path} ({media_type}, {} bytes)]", bytes.len());
    let (width, height) = crate::llm::parse_image_dimensions(&bytes)
        .map(|(w, h)| (Some(w), Some(h)))
        .unwrap_or_default();
    Ok(ToolOutput::with_images(
        note,
        vec![ImageData {
            media_type: media_type.to_string(),
            data,
            width,
            height,
            ..Default::default()
        }],
    ))
}

#[derive(Debug)]
struct BulkPattern {
    matcher: GlobMatcher,
    absolute: bool,
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn compile_bulk_pattern(raw: &str) -> Result<BulkPattern, String> {
    let absolute = Path::new(raw).is_absolute();
    let mut pattern = path_text(Path::new(raw));
    if !absolute {
        while let Some(stripped) = pattern.strip_prefix("./") {
            pattern = stripped.to_string();
        }
    }
    let matcher = GlobBuilder::new(&pattern)
        .literal_separator(true)
        .build()
        .map_err(|error| format!("invalid glob `{raw}`: {error}"))?
        .compile_matcher();
    Ok(BulkPattern { matcher, absolute })
}

impl BulkPattern {
    fn matches(&self, path: &Path, base: &Path) -> bool {
        let candidate = if self.absolute {
            path.to_path_buf()
        } else {
            path.strip_prefix(base).unwrap_or(path).to_path_buf()
        };
        self.matcher.is_match(candidate)
    }
}

fn bulk_walk_root(raw: &str, base: &Path) -> PathBuf {
    if !Path::new(raw).is_absolute() {
        return base.to_path_buf();
    }
    let mut root = PathBuf::new();
    for component in Path::new(raw).components() {
        let segment = component.as_os_str().to_string_lossy();
        if has_glob_magic(&segment) {
            break;
        }
        root.push(component.as_os_str());
    }
    if root.as_os_str().is_empty() {
        PathBuf::from(std::path::MAIN_SEPARATOR.to_string())
    } else {
        root
    }
}

fn is_hidden_bulk_path(path: &Path, base: &Path) -> bool {
    path.strip_prefix(base)
        .unwrap_or(path)
        .components()
        .any(|component| {
            component
                .as_os_str()
                .to_str()
                .is_some_and(|name| name.starts_with('.') && name != "." && name != "..")
        })
}

async fn bulk_base(working_dir: Option<&Path>) -> Result<PathBuf, String> {
    let base = working_dir
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    fs::canonicalize(base)
        .await
        .map_err(|error| format!("could not resolve bulk working directory: {error}"))
}

async fn expand_bulk_paths(
    args: &Args,
    working_dir: Option<&Path>,
) -> Result<Vec<PathBuf>, String> {
    let base = bulk_base(working_dir).await?;
    let mut patterns = Vec::with_capacity(1 + args.paths.len());
    patterns.push(args.path.as_str());
    patterns.extend(args.paths.iter().map(String::as_str));
    let exclusions = args
        .exclude
        .iter()
        .map(|pattern| compile_bulk_pattern(pattern))
        .collect::<Result<Vec<_>, _>>()?;
    let mut matched = BTreeSet::new();

    for raw in patterns {
        if has_glob_magic(raw) {
            let pattern = compile_bulk_pattern(raw)?;
            let root = bulk_walk_root(raw, &base);
            let walker = WalkBuilder::new(root)
                .standard_filters(true)
                .hidden(true)
                .git_ignore(true)
                .require_git(false)
                .build();
            for entry in walker {
                let Ok(entry) = entry else { continue };
                let Some(file_type) = entry.file_type() else {
                    continue;
                };
                if !file_type.is_file() || !pattern.matches(entry.path(), &base) {
                    continue;
                }
                if is_hidden_bulk_path(entry.path(), &base)
                    || exclusions
                        .iter()
                        .any(|exclude| exclude.matches(entry.path(), &base))
                {
                    continue;
                }
                let Ok(resolved) = fs::canonicalize(entry.path()).await else {
                    continue;
                };
                matched.insert(resolved);
            }
        } else {
            let resolved = snapshot::resolve_existing_path(raw, working_dir).await?;
            if is_hidden_bulk_path(&resolved, &base)
                || exclusions
                    .iter()
                    .any(|exclude| exclude.matches(&resolved, &base))
            {
                continue;
            }
            matched.insert(resolved);
        }
    }

    Ok(matched.into_iter().collect())
}

async fn read_bulk(
    args: &Args,
    snapshots: Option<&Snapshots>,
    working_dir: Option<&Path>,
) -> Result<ToolOutput, String> {
    if args.start_line.is_some() || args.max_lines.is_some() {
        return Err(
            "start_line and max_lines are only supported for a single literal path; remove them from a bulk/glob read or read each file individually"
                .to_string(),
        );
    }

    let paths = expand_bulk_paths(args, working_dir).await?;
    if paths.is_empty() {
        return Err(format!(
            "bulk read matched no visible regular files for `{}`",
            args.path
        ));
    }

    let total = paths.len();
    let mut items: Vec<ToolOutput> = Vec::new();
    let mut body_bytes = 0usize;
    for path in paths.iter().take(MAX_BULK_FILES) {
        let path_text = path.to_string_lossy().into_owned();
        let result = if let Some(media_type) = image_media_type(&path_text) {
            read_image_resolved(path, media_type).await
        } else {
            read_text(&Args::literal(path_text), snapshots, working_dir)
                .await
                .map(ToolOutput::text)
        };
        let Ok(output) = result else { continue };

        // Reserve room for the summary line and separators so the aggregate
        // never exceeds the cap, without reading files only to drop them.
        let prospective = items.len() + 1;
        let summary_len = format!(
            "Bulk read: returned {prospective} of {total} matched files; skipped {}.",
            total - prospective
        )
        .len();
        let added = 2 + output.content.len();
        if !items.is_empty() && summary_len + body_bytes + added > MAX_BULK_OUTPUT_BYTES {
            // This file was read (arming unchanged-read dedup) but is not
            // returned; drop its dedup entry so a later read is not stubbed.
            if let Some(store) = snapshots
                && let Ok(mut store) = store.write()
            {
                store.clear_last_read(&path.to_string_lossy());
            }
            break;
        }
        body_bytes += added;
        items.push(output);
    }

    let returned = items.len();
    let skipped = total - returned;
    let mut content =
        format!("Bulk read: returned {returned} of {total} matched files; skipped {skipped}.");
    for item in &items {
        content.push_str("\n\n");
        content.push_str(&item.content);
    }
    let images = items.into_iter().flat_map(|item| item.images).collect();
    Ok(ToolOutput::with_images(content, images))
}

async fn ensure_readable(path: &str, max_bytes: u64) -> Result<(), String> {
    snapshot::ensure_readable_regular_file(path, max_bytes).await
}

/// Read text, optionally recording the snapshot and shown line numbers.
async fn read_text(
    args: &Args,
    snapshots: Option<&Snapshots>,
    working_dir: Option<&Path>,
) -> Result<String, String> {
    let resolved = snapshot::resolve_existing_path(&args.path, working_dir).await?;
    let path = resolved.to_string_lossy().into_owned();
    ensure_readable(&path, MAX_TEXT_FILE_BYTES).await?;
    let raw = fs::read_to_string(&resolved).await.map_err(|e| {
        if e.kind() == ErrorKind::InvalidData {
            "file is not valid UTF-8 (probably binary); use shell to inspect it".to_string()
        } else {
            crate::util::errstr(e)
        }
    })?;
    ensure_len(raw.len() as u64, MAX_TEXT_FILE_BYTES)?;

    let normalized = snapshot::normalize_text(&raw);
    let lines = snapshot::numbered_lines(&normalized);
    let total = lines.len();

    let start = args.start_line.unwrap_or(1).saturating_sub(1);
    let max = args.max_lines.unwrap_or(DEFAULT_MAX_LINES).min(1000);

    let first = start + 1; // 1-based first line shown

    if first > total {
        // Range starts past EOF: nothing to show, but still report totals.
        record_snapshot(snapshots, &path, &raw, &normalized, &[], None)?;
        return Ok(if total > 0 {
            format!(
                "File: {path}\nRange: no lines; file has {total} line{}",
                plural(total)
            )
        } else {
            format!("File: {path}\nRange: empty file; 0 lines total")
        });
    }

    // Collect the requested window, bounding both individual lines and the
    // complete rendered tool output. Lines are never split by the output cap.
    let requested_end = (start + max).min(total);
    // Measure the widest header we could emit once, so the byte cap counts it
    // without rebuilding the header for every candidate line. The header is
    // longest when the read continues past the window (it then carries the
    // "Stopped"/"Continue" lines), and only grows with the shown line number and
    // truncated-line count, so sizing it for the whole window is a safe bound.
    let header_end = requested_end.min(total.saturating_sub(1)).max(first);
    let header_reserve = range_header(
        &path,
        first,
        header_end,
        total,
        Some("50 KiB output limit"),
        requested_end.saturating_sub(first) + 1,
    )
    .len();
    let mut end = start;
    let mut body = String::new();
    let mut shown_nums: Vec<usize> = Vec::with_capacity(requested_end - start);
    let mut truncated_count = 0usize;
    let mut byte_limited = false;
    for n in first..=requested_end {
        let content = lines[n - 1];
        let overlong = content.chars().count() > MAX_TOOL_LINE_CHARS;
        let rendered = if overlong {
            format!(
                "{n:>5} | {}  [not editable — line exceeds {MAX_TOOL_LINE_CHARS} chars]\n",
                truncate_line(content)
            )
        } else {
            format!("{n:>5} | {content}\n")
        };
        if !body.is_empty() && header_reserve + body.len() + rendered.len() > MAX_OUTPUT_BYTES {
            byte_limited = true;
            break;
        }
        body.push_str(&rendered);
        end = n;
        truncated_count += usize::from(overlong);
        if !overlong {
            shown_nums.push(n);
        }
    }

    let window = (!shown_nums.is_empty()).then_some((first, end));
    let (tag, unchanged) =
        record_snapshot(snapshots, &path, &raw, &normalized, &shown_nums, window)?;
    if unchanged {
        return Ok(format!(
            "File: {path}\nUnchanged: lines {first}-{end}, tag {tag} — identical to the earlier read; that content is already in context."
        ));
    }

    let stopped = if byte_limited {
        Some("50 KiB output limit")
    } else if end < total {
        Some(if args.max_lines.is_some() {
            "requested line limit"
        } else {
            "1,000-line limit"
        })
    } else {
        None
    };
    let header = range_header(&path, first, end, total, stopped, truncated_count);

    // `body` ends with a trailing newline; remove only that delimiter so
    // significant trailing spaces or tabs on the final displayed line survive.
    Ok(format!(
        "{header}{}",
        body.strip_suffix('\n').unwrap_or(&body)
    ))
}

/// Record the full snapshot and the 1-based line numbers shown to the model.
fn record_snapshot(
    snapshots: Option<&Snapshots>,
    path: &str,
    raw: &str,
    normalized: &str,
    seen: &[usize],
    window: Option<(usize, usize)>,
) -> Result<(String, bool), String> {
    let tag = snapshot::compute_tag(normalized);
    if let Some(store) = snapshots {
        let mut guard = store
            .write()
            .map_err(|_| "snapshot store lock is poisoned".to_string())?;
        let tag = guard.record_with_format(
            path,
            normalized,
            snapshot::TextFormat::detect(raw),
            Some(seen),
        );
        let unchanged = if let Some((first, end)) = window {
            let digest = snapshot::compute_digest(normalized);
            guard.take_unchanged(path, &digest, first, end)
        } else {
            guard.clear_last_read(path);
            false
        };
        return Ok((tag, unchanged));
    }
    Ok((tag, false))
}

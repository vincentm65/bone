//! The `read_file` tool: text and image reading with optional line ranges.
//!
//! Text is returned with a simple file/range header and numbered lines. The
//! full content and visible lines are recorded internally so `edit_file` can
//! validate an exact replacement without making the model repeat hashes or a
//! custom patch language. Image files are returned as attachments unchanged.

use std::path::Path;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use base64::Engine;
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::ErrorKind;
use tokio::fs;

use crate::llm::ImageData;
use crate::tools::snapshot::{self, SnapshotStore};
use crate::tools::types::{Tool, ToolDefinition, ToolExecutionContext, ToolOutput};
use crate::tools::{MAX_TOOL_LINE_CHARS, truncate_line};

/// Shared snapshot store type (mirrors the alias in `edit_file`).
type Snapshots = Arc<RwLock<SnapshotStore>>;

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
/// Default window when the model omits `max_lines`. High enough that typical
/// source files fit in one full read (safer first-try edits) while still
/// hard-capped at the schema maximum of 1000.
const DEFAULT_MAX_LINES: usize = 1000;
async fn ensure_size(path: &str, max_bytes: u64) -> Result<(), String> {
    let metadata = fs::metadata(path).await.map_err(crate::util::errstr)?;
    ensure_len(metadata.len(), max_bytes)
}

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

#[derive(Deserialize)]
struct Args {
    path: String,
    start_line: Option<usize>,
    max_lines: Option<usize>,
}

#[async_trait]
impl Tool for ReadFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read_file".to_string(),
            description:
                "Preferred tool for reading file contents; use this instead of shell commands such as cat, head, tail, or sed. Reads a UTF-8 text file and returns the resolved path, range information, and numbered lines. To edit, copy an exact unique block of shown text into edit_file.old_text and provide its replacement as new_text. Optionally pass start_line and max_lines; defaults to the first 1000 lines. Image files (png, jpg, jpeg, gif, webp) are returned as an image you can view."
                    .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path to read. Relative paths resolve from the working directory."
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
        let args: Args = serde_json::from_value(arguments).map_err(crate::util::errstr)?;
        read_text(&args, None, None).await
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
        let path = arguments.get("path").and_then(|v| v.as_str());
        if let Some(media_type) = path.and_then(image_media_type) {
            let resolved = snapshot::resolve_existing_path(path.unwrap(), working_dir).await?;
            let path = resolved.to_string_lossy().into_owned();
            ensure_size(&path, MAX_IMAGE_FILE_BYTES).await?;
            let bytes = fs::read(&resolved).await.map_err(crate::util::errstr)?;
            ensure_len(bytes.len() as u64, MAX_IMAGE_FILE_BYTES)?;
            let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
            let note = format!("[read image {path} ({media_type}, {} bytes)]", bytes.len());
            return Ok(ToolOutput::with_images(
                note,
                vec![ImageData {
                    media_type: media_type.to_string(),
                    data,
                }],
            ));
        }

        let args: Args = serde_json::from_value(arguments).map_err(crate::util::errstr)?;
        let text = read_text(&args, snapshots, working_dir).await?;
        Ok(ToolOutput::text(text))
    }
}

/// Read text, optionally recording the snapshot and shown line numbers.
async fn read_text(
    args: &Args,
    snapshots: Option<&Snapshots>,
    working_dir: Option<&Path>,
) -> Result<String, String> {
    let resolved = snapshot::resolve_existing_path(&args.path, working_dir).await?;
    let path = resolved.to_string_lossy().into_owned();
    ensure_size(&path, MAX_TEXT_FILE_BYTES).await?;
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
        record_snapshot(snapshots, &path, &normalized, &[])?;
        return Ok(if total > 0 {
            format!(
                "File: {path}\nRange: no lines; file has {total} line{}",
                plural(total)
            )
        } else {
            format!("File: {path}\nRange: empty file; 0 lines total")
        });
    }

    // Collect the requested window, bounding per-line byte cost so a single
    // minified multi-MB line can't consume the whole context window. Truncated
    // lines are shown for orientation but excluded from the editable set so
    // the model cannot invent a body from a partial view.
    let end = (start + max).min(total);
    let mut body = String::new();
    let mut shown_nums: Vec<usize> = Vec::with_capacity(end - start);
    let mut truncated_count = 0usize;
    for n in first..=end {
        let content = lines[n - 1];
        let overlong = content.chars().count() > MAX_TOOL_LINE_CHARS;
        if overlong {
            truncated_count += 1;
            body.push_str(&format!(
                "{n:>5} | {}  [not editable — line exceeds {MAX_TOOL_LINE_CHARS} chars]\n",
                truncate_line(content)
            ));
        } else {
            body.push_str(&format!("{n:>5} | {content}\n"));
            shown_nums.push(n);
        }
    }

    // Record the full normalized text + only the shown (editable) line numbers.
    // Elided and truncated lines are not editable (visible-line guard).
    record_snapshot(snapshots, &path, &normalized, &shown_nums)?;

    // Header + footer around the numbered lines.
    let mut header = format!("File: {path}\n");
    if end < total {
        header.push_str(&format!(
            "Range: lines {first}-{end} of {total}. To continue, call read_file with start_line={next}.\n",
            next = end + 1,
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
            "Note: {truncated_count} overlong line{} truncated and not editable; use shell to inspect it.\n",
            plural(truncated_count),
        ));
    }

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
    normalized: &str,
    seen: &[usize],
) -> Result<(), String> {
    if let Some(store) = snapshots {
        let mut guard = store
            .write()
            .map_err(|_| "snapshot store lock is poisoned".to_string())?;
        guard.record(path, normalized, Some(seen));
    }
    Ok(())
}

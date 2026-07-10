//! The `read_file` tool: text and image reading with optional line ranges.
//!
//! Text files are always emitted in hashline form — a `[path#TAG]` header
//! followed by `N:`-prefixed lines — so the model can reference exact line
//! numbers in a subsequent `edit_file` patch. Each read records the file's
//! normalized full text plus the set of line numbers actually shown
//! ([`crate::tools::snapshot::SnapshotStore`]); only those lines are
//! "editable", and the visible-line guard in `edit_file` rejects edits to
//! elided lines. Image files are returned as attachments unchanged.

use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use base64::Engine;
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::ErrorKind;
use tokio::fs;

use crate::llm::ImageData;
use crate::tools::snapshot::{self, SnapshotStore};
use crate::tools::truncate_line;
use crate::tools::types::{Tool, ToolDefinition, ToolExecutionContext, ToolOutput};

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

async fn ensure_size(path: &str, max_bytes: u64) -> Result<(), String> {
    let metadata = fs::metadata(path).await.map_err(crate::util::errstr)?;
    let len = metadata.len();
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
                "Read a UTF-8 text file in hashline form: a `[path#TAG]` header then `N:`-prefixed lines. Optionally pass start_line and max_lines to read a range; only shown lines are editable in a later edit_file. Image files (png, jpg, jpeg, gif, webp) are returned as an image you can view."
                    .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path to read."
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
                        "description": "Max lines to return. Defaults to 500."
                    }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(&self, arguments: Value) -> Result<String, String> {
        let args: Args = serde_json::from_value(arguments).map_err(crate::util::errstr)?;
        read_text_hashline(&args, None).await
    }

    async fn execute_output(&self, arguments: Value) -> Result<ToolOutput, String> {
        self.read_file_inner(arguments, None).await
    }

    async fn execute_output_live(
        &self,
        arguments: Value,
        _events: Option<tokio::sync::mpsc::UnboundedSender<crate::pane_content::KeyRequest>>,
        context: ToolExecutionContext,
    ) -> Result<ToolOutput, String> {
        self.read_file_inner(arguments, Some(&context.snapshots))
            .await
    }
}

impl ReadFileTool {
    /// Read a file. Image files become attachments; text files take the
    /// hashline path (recording the snapshot when a store is provided).
    async fn read_file_inner(
        &self,
        arguments: Value,
        snapshots: Option<&Snapshots>,
    ) -> Result<ToolOutput, String> {
        let path = arguments.get("path").and_then(|v| v.as_str());
        if let Some(media_type) = path.and_then(image_media_type) {
            let path = path.unwrap().to_string();
            ensure_size(&path, MAX_IMAGE_FILE_BYTES).await?;
            let bytes = fs::read(&path).await.map_err(crate::util::errstr)?;
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
        let text = read_text_hashline(&args, snapshots).await?;
        Ok(ToolOutput::text(text))
    }
}

/// Read a text file and format it as hashline output (`[path#TAG]` + `N:`
/// lines), optionally recording the snapshot + shown line numbers.
async fn read_text_hashline(args: &Args, snapshots: Option<&Snapshots>) -> Result<String, String> {
    ensure_size(&args.path, MAX_TEXT_FILE_BYTES).await?;
    let raw = fs::read_to_string(&args.path).await.map_err(|e| {
        if e.kind() == ErrorKind::InvalidData {
            "file is not valid UTF-8 (probably binary); use shell to inspect it".to_string()
        } else {
            crate::util::errstr(e)
        }
    })?;

    let normalized = snapshot::normalize_text(&raw);
    let lines = snapshot::numbered_lines(&normalized);
    let total = lines.len();

    let start = args.start_line.unwrap_or(1).saturating_sub(1);
    let max = args.max_lines.unwrap_or(500).min(1000);

    let first = start + 1; // 1-based first line shown

    if first > total {
        // Range starts past EOF: nothing to show, but still report totals.
        let tag = record_or_tag(snapshots, &args.path, &normalized, &[]);
        return Ok(if total > 0 {
            format!(
                "[{}#{tag}]\n[no lines in range; file has {total} line{}]",
                args.path,
                plural(total)
            )
        } else {
            format!("[{}#{tag}]\n[empty file; 0 lines total]", args.path)
        });
    }

    // Collect the requested window, bounding per-line byte cost so a single
    // minified multi-MB line can't consume the whole context window.
    let end = (start + max).min(total);
    let mut body = String::new();
    let mut shown_nums: Vec<usize> = Vec::with_capacity(end - start);
    for n in first..=end {
        body.push_str(&format!("{n:>5}: {}\n", truncate_line(lines[n - 1])));
        shown_nums.push(n);
    }

    // Record the full normalized text + only the shown line numbers. Edits may
    // anchor on shown lines; elided lines are not editable (visible-line guard).
    let tag = record_or_tag(snapshots, &args.path, &normalized, &shown_nums);

    // Header + footer around the numbered lines.
    let mut header = format!("[{}#{tag}]\n", args.path);
    if end < total {
        header.push_str(&format!(
            "[showing lines {first}-{end} of {total}; only these lines are editable — \
             call read_file again with start_line={next} to see the rest]\n",
            next = end + 1,
        ));
    } else if first > 1 {
        header.push_str(&format!(
            "[showing lines {first}-{end} of {total}; end of file]\n"
        ));
    } else {
        header.push_str(&format!("[{total} line{} total]\n", plural(total)));
    }

    // `body` ends with a trailing newline; trim it so the block reads cleanly.
    Ok(format!("{header}{}", body.trim_end()))
}

/// Record the snapshot (when a store is present) and return its tag; otherwise
/// just compute the tag. `seen` is the 1-based line numbers actually shown.
fn record_or_tag(
    snapshots: Option<&Snapshots>,
    path: &str,
    normalized: &str,
    seen: &[usize],
) -> String {
    match snapshots {
        Some(store) => match store.write() {
            Ok(mut guard) => guard.record(path, normalized, Some(seen)),
            Err(_) => snapshot::compute_tag(normalized),
        },
        None => snapshot::compute_tag(normalized),
    }
}

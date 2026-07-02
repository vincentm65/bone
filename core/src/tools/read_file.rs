//! The `read_file` tool: text and image reading with optional line ranges.

use async_trait::async_trait;
use base64::Engine;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::fs;

use crate::llm::ImageData;
use crate::tools::types::{Tool, ToolDefinition, ToolOutput};

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

/// Cap an individual line's length so a single minified multi-MB line can't
/// consume the whole context window. Truncates on a UTF-8 char boundary.
const MAX_LINE_CHARS: usize = 2000;
fn truncate_line(line: &str) -> String {
    if line.chars().count() <= MAX_LINE_CHARS {
        return line.to_string();
    }
    // Byte offset of the char at index MAX_LINE_CHARS is a valid boundary.
    let end = line
        .char_indices()
        .nth(MAX_LINE_CHARS)
        .map(|(offset, _)| offset)
        .unwrap_or(line.len());
    let mut out = line[..end].to_string();
    out.push_str("…[truncated]");
    out
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
                "Read a UTF-8 text file. Optionally pass start_line and max_lines to read a range. \
                 Image files (png, jpg, jpeg, gif, webp) are returned as an image you can view."
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
        let content = fs::read_to_string(&args.path)
            .await
            .map_err(crate::util::errstr)?;

        let start = args.start_line.unwrap_or(1).saturating_sub(1);
        let max = args.max_lines.unwrap_or(500).min(1000);

        // Single pass over the file's lines: count the total and keep only the
        // requested window, so slicing a large file never materializes every
        // line (the old `lines().collect()` allocated a Vec of every &str).
        let mut total = 0;
        let mut shown: Vec<String> = Vec::with_capacity(max);
        for (i, line) in content.lines().enumerate() {
            total = i + 1;
            if i >= start && shown.len() < max {
                // Bound per-line byte cost so a single minified multi-MB line
                // can't consume the whole context window.
                shown.push(truncate_line(line));
            }
        }

        let first = start + 1; // 1-based first line shown
        let end = start + shown.len(); // 1-based last line shown

        if shown.is_empty() {
            // Nothing in range (e.g. start_line past EOF): still report totals.
            return Ok(if total > 0 {
                format!(
                    "[no lines in range; file has {total} line{}]",
                    plural(total)
                )
            } else {
                String::new()
            });
        }

        let mut out = shown.join("\n");
        if end < total {
            // Partial view: tell the model exactly where it is and how to page.
            out.push_str(&format!(
                "\n\n[showing lines {first}-{end} of {total}; call again with start_line={next} to continue]",
                next = end + 1,
            ));
        } else {
            // Complete view: give size awareness.
            out.push_str(&format!("\n\n[{total} line{} total]", plural(total)));
        }
        Ok(out)
    }

    async fn execute_output(&self, arguments: Value) -> Result<ToolOutput, String> {
        let path = arguments.get("path").and_then(|v| v.as_str());
        // Image files are read as bytes and returned as an attachment for
        // vision-capable models, rather than as (binary) text.
        if let Some(media_type) = path.and_then(image_media_type) {
            let path = path.unwrap().to_string();
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
        self.execute(arguments).await.map(ToolOutput::text)
    }
}

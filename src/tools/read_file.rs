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
        let args: Args = serde_json::from_value(arguments).map_err(|e| e.to_string())?;
        let content = fs::read_to_string(&args.path)
            .await
            .map_err(|e| e.to_string())?;

        let start = args.start_line.unwrap_or(1).saturating_sub(1);
        let max = args.max_lines.unwrap_or(500).min(1000);
        let lines: Vec<&str> = content.lines().skip(start).take(max).collect();
        Ok(lines.join("\n"))
    }

    async fn execute_output(&self, arguments: Value) -> Result<ToolOutput, String> {
        let path = arguments.get("path").and_then(|v| v.as_str());
        // Image files are read as bytes and returned as an attachment for
        // vision-capable models, rather than as (binary) text.
        if let Some(media_type) = path.and_then(image_media_type) {
            let path = path.unwrap().to_string();
            let bytes = fs::read(&path).await.map_err(|e| e.to_string())?;
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

//! The `write_file` tool: creates a new file atomically (fails if it exists).

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::ErrorKind;
use std::path::Path;
use tokio::fs;

use crate::tools::types::{Tool, ToolDefinition};
use crate::tools::write_atomic::write_atomic;

pub struct WriteFileTool;

#[derive(Deserialize)]
struct Args {
    path: String,
    content: String,
}

#[async_trait]
impl Tool for WriteFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "write_file".to_string(),
            description: "Create a new UTF-8 text file. Fails if the file already exists — use edit_file for modifications (mode=\"rewrite\" for a full rewrite).".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path. Parent directories created automatically."
                    },
                    "content": {
                        "type": "string",
                        "description": "File contents to write."
                    }
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(&self, arguments: Value) -> Result<String, String> {
        let args: Args = serde_json::from_value(arguments).map_err(crate::util::errstr)?;
        if let Some(parent) = Path::new(&args.path).parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)
                .await
                .map_err(crate::util::errstr)?;
        }
        let path = Path::new(&args.path);
        // Reject if the destination already exists. `rename` will silently
        // overwrite on Unix, and `exists()` misses dangling symlinks, so use
        // symlink_metadata. A create-between-check-and-rename race remains but
        // is acceptable for this tool's local convenience threat model.
        match fs::symlink_metadata(path).await {
            Ok(_) => {
                return Err(
                    "file already exists; use edit_file (search/replace for targeted changes, or mode=\"rewrite\" for a full rewrite)"
                        .to_string(),
                );
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(crate::util::errstr(e)),
        }
        write_atomic(path, &args.content, None).await?;
        Ok(format!("wrote {} bytes", args.content.len()))
    }
}

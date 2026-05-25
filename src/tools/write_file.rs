use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
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
            name: "write_file",
            description: "Create a new UTF-8 text file. Parent directories are created automatically, but the call fails if the destination file already exists. Use edit_file for targeted modifications to existing files.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Destination file path. Relative paths are resolved from the current working directory. Parent directories will be created if needed."
                    },
                    "content": {
                        "type": "string",
                        "description": "Full UTF-8 file contents to write. The call fails rather than overwriting if the file already exists."
                    }
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(&self, arguments: Value) -> Result<String, String> {
        let args: Args = serde_json::from_value(arguments).map_err(|e| e.to_string())?;
        if let Some(parent) = Path::new(&args.path).parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| e.to_string())?;
        }
        let path = Path::new(&args.path);
        // Reject if the destination already exists — we use create_new on a temp
        // file, but rename will silently overwrite on Unix.  Check up-front so
        // the caller gets a clear error.
        if path.exists() {
            return Err(
                "file already exists; use edit_file for targeted modifications".to_string(),
            );
        }
        write_atomic(path, &args.content, None).await?;
        Ok(format!("wrote {} bytes", args.content.len()))
    }
}

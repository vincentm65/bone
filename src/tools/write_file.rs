use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;
use tokio::fs;

use crate::tools::types::{Tool, ToolDefinition};

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
            description: "Create or overwrite a UTF-8 text file.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(&self, arguments: Value) -> Result<String, String> {
        let args: Args = serde_json::from_value(arguments).map_err(|e| e.to_string())?;
        if let Some(parent) = Path::new(&args.path).parent()
            && !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).await.map_err(|e| e.to_string())?;
            }
        fs::write(&args.path, args.content.as_bytes()).await.map_err(|e| e.to_string())?;
        Ok(format!("wrote {} bytes", args.content.len()))
    }
}

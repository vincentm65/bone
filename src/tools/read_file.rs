use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::fs;

use crate::tools::types::{Tool, ToolDefinition};

pub struct ReadFileTool;

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
            name: "read_file",
            description: "Read a UTF-8 text file. Use this to inspect source, config, or notes before editing. Optionally pass start_line and max_lines to read only a range.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to read. Relative paths are resolved from the current working directory; absolute paths are allowed when accessible."
                    },
                    "start_line": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Optional 1-based first line to include. Omit to start at line 1."
                    },
                    "max_lines": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 1000,
                        "description": "Optional maximum number of lines to return. Use this to avoid dumping large files; defaults to 200 when start_line or max_lines is provided."
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

        if args.start_line.is_none() && args.max_lines.is_none() {
            return Ok(content);
        }

        let start = args.start_line.unwrap_or(1).saturating_sub(1);
        let max = args.max_lines.unwrap_or(200).min(1000);
        Ok(content
            .lines()
            .skip(start)
            .take(max)
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

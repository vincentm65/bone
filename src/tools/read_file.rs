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
            name: "read_file".to_string(),
            description:
                "Read a UTF-8 text file. Optionally pass start_line and max_lines to read a range."
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
}

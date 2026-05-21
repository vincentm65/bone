use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::fs;

use crate::tools::types::{Tool, ToolDefinition};

pub struct EditFileTool;

#[derive(Deserialize)]
struct Args {
    path: String,
    search: String,
    replace: String,
}

#[async_trait]
impl Tool for EditFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "edit_file",
            description: "Replace one exact text occurrence in a UTF-8 file. Fails unless the search text appears exactly once.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "search": { "type": "string" },
                    "replace": { "type": "string" }
                },
                "required": ["path", "search", "replace"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(&self, arguments: Value) -> Result<String, String> {
        let args: Args = serde_json::from_value(arguments).map_err(|e| e.to_string())?;
        if args.search.is_empty() {
            return Err("search must not be empty".to_string());
        }

        let content = fs::read_to_string(&args.path)
            .await
            .map_err(|e| e.to_string())?;
        let count = content.matches(&args.search).count();
        if count != 1 {
            return Err(format!(
                "search text matched {count} times; expected exactly 1"
            ));
        }

        let next = content.replacen(&args.search, &args.replace, 1);
        fs::write(&args.path, next.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        Ok("edited 1 occurrence".to_string())
    }
}

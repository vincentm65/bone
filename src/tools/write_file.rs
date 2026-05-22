use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::{fs, io::AsyncWriteExt};

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
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_else(|_| std::process::id() as u128);
        let pid = std::process::id();
        let temp_path = path.with_extension(format!("bone-tmp-{pid}-{nanos}"));
        // Atomically create a new temp file; create_new(true) ensures we
        // never clobber an existing file from a crashed prior invocation.
        {
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp_path)
                .await
                .map_err(|e| e.to_string())?;
            f.write_all(args.content.as_bytes())
                .await
                .map_err(|e| e.to_string())?;
            f.flush().await.map_err(|e| e.to_string())?;
        }
        fs::rename(&temp_path, path).await.map_err(|e| {
            let _ = std::fs::remove_file(&temp_path);
            e.to_string()
        })?;
        Ok(format!("wrote {} bytes", args.content.len()))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::json;
    use tokio::fs;

    use super::WriteFileTool;
    use crate::tools::types::Tool;

    fn temp_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("bone-write-file-{name}-{nanos}"))
    }

    #[tokio::test]
    async fn creates_new_file() {
        let path = temp_path("creates").join("nested/file.txt");
        let tool = WriteFileTool;

        let result = tool
            .execute(json!({ "path": path, "content": "hello" }))
            .await
            .expect("write_file should create a new file");

        assert_eq!(result, "wrote 5 bytes");
        assert_eq!(
            fs::read_to_string(&path)
                .await
                .expect("created file should be readable"),
            "hello"
        );

        // Clean up temp dir — best effort
        if let Some(grandparent) = path.parent().and_then(|p| p.parent()) {
            let _ = fs::remove_dir_all(grandparent).await;
        }
    }

    #[tokio::test]
    async fn refuses_to_overwrite_existing_file() {
        let path = temp_path("exists.txt");
        fs::write(&path, "original")
            .await
            .expect("test setup should create existing file");
        let tool = WriteFileTool;

        let result = tool
            .execute(json!({ "path": path, "content": "replacement" }))
            .await;

        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(&path)
                .await
                .expect("existing file should remain readable"),
            "original"
        );

        let _ = fs::remove_file(path).await;
    }
}

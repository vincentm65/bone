use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::process::Stdio;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

use crate::tools::types::{Tool, ToolDefinition};

pub struct BashTool;

#[derive(Deserialize)]
struct Args {
    command: String,
    timeout_ms: Option<u64>,
}

#[async_trait]
impl Tool for BashTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "bash",
            description: "Run a shell command from the current working directory and return exit code, stdout, and stderr.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "timeout_ms": { "type": "integer", "minimum": 1000, "maximum": 300000 }
                },
                "required": ["command"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(&self, arguments: Value) -> Result<String, String> {
        let args: Args = serde_json::from_value(arguments).map_err(|e| e.to_string())?;
        let timeout_ms = args.timeout_ms.unwrap_or(120_000).clamp(1_000, 300_000);

        let mut child = Command::new("bash")
            .arg("-lc")
            .arg(args.command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;

        let mut stdout = child.stdout.take().ok_or("failed to capture stdout")?;
        let mut stderr = child.stderr.take().ok_or("failed to capture stderr")?;

        let wait = async {
            let mut out = Vec::new();
            let mut err = Vec::new();
            let status_fut = child.wait();
            let out_fut = stdout.read_to_end(&mut out);
            let err_fut = stderr.read_to_end(&mut err);
            let (status, _, _) = tokio::try_join!(status_fut, out_fut, err_fut).map_err(|e| e.to_string())?;
            Ok::<_, String>((status, out, err))
        };

        let (status, out, err) = match timeout(Duration::from_millis(timeout_ms), wait).await {
            Ok(result) => result?,
            Err(_) => return Err(format!("command timed out after {timeout_ms}ms")),
        };

        Ok(format!(
            "exit code: {}\nstdout:\n{}\nstderr:\n{}",
            status.code().map_or_else(|| "signal".to_string(), |code| code.to_string()),
            String::from_utf8_lossy(&out),
            String::from_utf8_lossy(&err),
        ))
    }
}

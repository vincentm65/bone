use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::process::Stdio;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time::{Duration, timeout};

use crate::tools::types::{Tool, ToolDefinition};

pub struct BashTool;

#[derive(Deserialize)]
struct Args {
    command: String,
    /// Classification from the model — accepted for schema compatibility but ignored.
    /// The deterministic classifier in command_policy is the sole authority.
    classification: Value,
    timeout_ms: Option<u64>,
}


#[async_trait]
impl Tool for BashTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "bash",
            description: "Run a non-interactive shell command with bash -lc from the current working directory and return its exit code, stdout, and stderr. Use this for builds, tests, formatters, package managers, and other commands that are better expressed in the shell. Do not use it to read or edit files when a dedicated file tool is more appropriate. Always classify the command honestly as read_only, edit, or danger; choose danger when unsure.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "Command line to execute with bash -lc. It runs without stdin, so avoid interactive prompts and provide flags that make tools non-interactive."
                    },
                    "classification": {
                        "type": "string",
                        "enum": ["read_only", "edit", "danger"],
                        "description": "Safety classification. Use read_only only for local inspection commands that do not modify files, services, network state, git state, or external systems, such as pwd, cargo check, cargo test, or listing files. Use edit for normal workspace mutations such as formatters, code generation, dependency installation, or creating/removing build artifacts. Use danger for privileged, destructive, external side-effecting, network-modifying, process/service-control, secret-accessing, or any git command, and whenever unsure."
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "minimum": 1000,
                        "maximum": 300000,
                        "description": "Optional timeout in milliseconds. Defaults to 120000 and is clamped between 1000 and 300000."
                    }
                },
                "required": ["command", "classification"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(&self, arguments: Value) -> Result<String, String> {
        let args: Args = serde_json::from_value(arguments).map_err(|e| e.to_string())?;
        let _ = args.classification;
        let timeout_ms = args.timeout_ms.unwrap_or(120_000).clamp(1_000, 300_000);

        let mut child = Command::new("bash")
            .arg("-lc")
            .arg(args.command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
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
            let (status, _, _) =
                tokio::try_join!(status_fut, out_fut, err_fut).map_err(|e| e.to_string())?;
            Ok::<_, String>((status, out, err))
        };

        let (status, out, err) = match timeout(Duration::from_millis(timeout_ms), wait).await {
            Ok(result) => result?,
            Err(_) => return Err(format!("command timed out after {timeout_ms}ms")),
        };

        let stdout_str = String::from_utf8_lossy(&out);
        let stderr_str = String::from_utf8_lossy(&err);
        let stdout_trunc = truncate_output(&stdout_str, 500);
        let stderr_trunc = truncate_output(&stderr_str, 100);

        Ok(format!(
            "exit code: {}\nstdout:\n{stdout_trunc}\nstderr:\n{stderr_trunc}",
            status
                .code()
                .map_or_else(|| "signal".to_string(), |code| code.to_string()),
        ))
    }
}

/// Truncate output to `max_lines`, keeping the first half and last half with a
/// marker showing how many lines were omitted.
pub fn truncate_output(output: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() <= max_lines {
        return output.to_string();
    }
    let head = max_lines / 2;
    let tail = max_lines - head;
    let mut out: Vec<&str> = lines[..head].to_vec();
    let truncated = format!("... {} lines truncated ...", lines.len() - max_lines);
    out.push(&truncated);
    out.extend_from_slice(&lines[lines.len() - tail..]);
    out.join("\n")
}

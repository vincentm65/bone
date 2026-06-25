//! The `shell` / `bash` tool: runs commands with streaming output and timeouts.

use std::process::Stdio;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time::{Duration, timeout};

use crate::tools::types::{Tool, ToolDefinition};

// ── Script execution (formerly script_runner.rs) ────────────────────────────

#[cfg(unix)]
unsafe extern "C" {
    fn setsid() -> i32;
}

pub struct ScriptRequest {
    pub command: String,
    pub env: Vec<(String, String)>,
    pub timeout_ms: u64,
}

pub struct ScriptOutput {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Returns the shell program, its argument flag, and a label for descriptions.
pub fn shell_command() -> (&'static str, &'static str, &'static str) {
    if cfg!(windows) {
        if which("pwsh") {
            ("pwsh", "-Command", "pwsh -Command")
        } else {
            ("powershell", "-Command", "powershell -Command")
        }
    } else {
        ("bash", "-lc", "bash -lc")
    }
}

fn which(name: &str) -> bool {
    std::process::Command::new(name)
        .arg("-Version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

pub async fn run_script(request: ScriptRequest) -> Result<ScriptOutput, String> {
    let timeout_ms = request.timeout_ms.clamp(1_000, 300_000);
    let (shell, shell_arg, _) = shell_command();
    let mut cmd = Command::new(shell);
    cmd.arg(shell_arg)
        .arg(&request.command)
        .envs(request.env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Detach controlling tty so sudo/ssh (reading /dev/tty) fail cleanly
    // instead of corrupting the TUI and swallowing keystrokes.
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| {
            setsid();
            Ok(())
        });
    }
    let mut child = cmd.spawn().map_err(crate::util::errstr)?;

    let mut stdout = child.stdout.take().ok_or("failed to capture stdout")?;
    let mut stderr = child.stderr.take().ok_or("failed to capture stderr")?;

    let wait = async {
        let status_fut = async { child.wait().await.map_err(crate::util::errstr) };
        let out_fut = async {
            let mut out = Vec::new();
            stdout
                .read_to_end(&mut out)
                .await
                .map_err(crate::util::errstr)?;
            Ok::<_, String>(out)
        };
        let err_fut = async {
            let mut err = Vec::new();
            stderr
                .read_to_end(&mut err)
                .await
                .map_err(crate::util::errstr)?;
            Ok::<_, String>(err)
        };
        let (status, out, err) = tokio::try_join!(status_fut, out_fut, err_fut)?;
        Ok::<_, String>((status, out, err))
    };

    let (status, out, err) = match timeout(Duration::from_millis(timeout_ms), wait).await {
        Ok(result) => result?,
        Err(_) => return Err(format!("command timed out after {timeout_ms}ms")),
    };

    Ok(ScriptOutput {
        exit_code: status.code(),
        stdout: truncate_output(&String::from_utf8_lossy(&out), 500),
        stderr: truncate_output(&String::from_utf8_lossy(&err), 100),
    })
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

// ── Shell tool ──────────────────────────────────────────────────────────────

pub struct ShellTool;

#[derive(Deserialize)]
struct Args {
    command: String,
    /// Classification from the model — accepted for schema compatibility but ignored.
    /// The deterministic classifier in command_policy is the sole authority.
    classification: Value,
    timeout_ms: Option<u64>,
}

#[async_trait]
impl Tool for ShellTool {
    fn definition(&self) -> ToolDefinition {
        let (_, _, shell_label) = shell_command();
        let desc = format!(
            "Run a non-interactive shell command with {shell_label}. Returns exit code, stdout, and stderr."
        );
        let cmd_desc = format!("Command to execute with {shell_label}. Runs without stdin.");
        ToolDefinition {
            name: "shell".to_string(),
            description: desc,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": cmd_desc,
                    },
                    "classification": {
                        "type": "string",
                        "enum": ["read_only", "danger"],
                        "description": "read_only or danger. Choose danger when unsure."
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "minimum": 1000,
                        "maximum": 300000,
                        "description": "Timeout in ms. Default 120000."
                    }
                },
                "required": ["command", "classification"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(&self, arguments: Value) -> Result<String, String> {
        let args: Args = serde_json::from_value(arguments).map_err(crate::util::errstr)?;
        let _ = args.classification;
        let timeout_ms = args.timeout_ms.unwrap_or(120_000).clamp(1_000, 300_000);

        let output = run_script(ScriptRequest {
            command: args.command,
            env: Vec::new(),
            timeout_ms,
        })
        .await?;
        let mut result = format!(
            "exit code: {}\nstdout:\n{stdout_trunc}",
            output
                .exit_code
                .map_or_else(|| "signal".to_string(), |code| code.to_string()),
            stdout_trunc = output.stdout,
        );
        if !output.stderr.is_empty() {
            result.push_str(&format!("\nstderr:\n{}", output.stderr));
        }
        Ok(result)
    }
}

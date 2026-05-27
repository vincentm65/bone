use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::tools::script_runner::{ScriptRequest, run_script, shell_command};
use crate::tools::types::{Tool, ToolDefinition};

pub use crate::tools::script_runner::truncate_output;

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
            "Run a non-interactive shell command with {shell_label} from the current working directory and return its exit code, stdout, and stderr. Use this for builds, tests, formatters, package managers, and other commands that are better expressed in the shell. Do not use it to read or edit files when a dedicated file tool is more appropriate. Always classify the command honestly as read_only, edit, or danger; choose danger when unsure."
        );
        let cmd_desc = format!(
            "Command line to execute with {shell_label}. It runs without stdin, so avoid interactive prompts and provide flags that make tools non-interactive."
        );
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

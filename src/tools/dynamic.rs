use async_trait::async_trait;
use ratatui::text::Line;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::tools::command_policy::CommandSafety;
use crate::tools::script_runner::{ScriptRequest, run_script};
use crate::tools::types::{Tool, ToolDefinition, ToolDisplayConfig, ToolOutput};
use crate::ui::pane_page::PanePage;
use crate::ui::render::DEFAULT_PANE_ROWS;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynamicTool {
    pub name: String,
    pub version: Option<u32>,
    pub description: String,
    #[serde(default)]
    pub args: Vec<ToolArg>,
    #[serde(default)]
    pub script: Option<String>,
    #[serde(default)]
    pub interaction: Option<InteractionType>,
    #[serde(default)]
    pub output: Option<OutputConfig>,
    #[serde(default)]
    pub safety: Option<CommandSafety>,
    #[serde(default)]
    pub display: Option<ToolDisplayConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionType {
    Select,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputConfig {
    pub kind: OutputKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OutputKind {
    JsonEnvelope,
}

#[derive(Debug, Deserialize)]
struct JsonEnvelope {
    content: String,
    #[serde(default)]
    pane: Option<PaneEnvelope>,
}

#[derive(Debug, Deserialize)]
struct PaneEnvelope {
    source: String,
    title: String,
    #[serde(default)]
    lines: Vec<String>,
    #[serde(default = "default_pane_rows")]
    visible_rows: usize,
    #[serde(default)]
    scroll: usize,
}

fn default_pane_rows() -> usize {
    DEFAULT_PANE_ROWS
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolArg {
    pub name: String,
    #[serde(rename = "type")]
    pub arg_type: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub required: bool,
}

impl DynamicTool {
    fn validate(&self) -> Result<(), String> {
        if self.interaction.is_some() && self.script.is_some() {
            return Err("interaction tools cannot define a script".to_string());
        }

        if let Some(InteractionType::Select) = self.interaction {
            let question = self.args.iter().find(|arg| arg.name == "question");
            if !matches!(question, Some(arg) if arg.arg_type == "string" && arg.required) {
                return Err(
                    "interaction: select requires a required string argument named question"
                        .to_string(),
                );
            }

            let options = self.args.iter().find(|arg| arg.name == "options");
            if !matches!(options, Some(arg) if arg.arg_type == "array" && arg.required) {
                return Err(
                    "interaction: select requires a required array argument named options"
                        .to_string(),
                );
            }
        }

        Ok(())
    }

    fn build_schema(&self) -> Value {
        let mut properties = serde_json::Map::new();
        let mut required = Vec::new();

        for arg in &self.args {
            let schema_type = match arg.arg_type.as_str() {
                "number" | "integer" => "number",
                "boolean" => "boolean",
                "array" => "array",
                _ => "string",
            };
            let mut prop = json!({
                "type": schema_type,
                "description": arg.description,
            });
            if arg.arg_type == "array" {
                prop["items"] = json!({ "type": "string" });
            }
            properties.insert(arg.name.clone(), prop);
            if arg.required {
                required.push(arg.name.clone());
            }
        }

        json!({
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        })
    }

    fn arg_to_env_name(name: &str) -> String {
        format!(
            "TOOL_{}",
            name.to_uppercase()
                .replace(|c: char| !c.is_alphanumeric(), "_")
        )
    }
}

#[async_trait]
impl Tool for DynamicTool {
    fn definition(&self) -> ToolDefinition {
        let mut desc = self.description.clone();
        if self.interaction.is_some() {
            desc.push_str(" (interaction tool)");
        }
        ToolDefinition {
            name: self.name.clone(),
            description: desc,
            input_schema: self.build_schema(),
        }
    }

    async fn execute(&self, arguments: Value) -> Result<String, String> {
        self.run(arguments).await.map(|output| output.stdout)
    }

    async fn execute_output(&self, arguments: Value) -> Result<ToolOutput, String> {
        let output = self.run(arguments).await?;
        match self.output.as_ref().map(|output| &output.kind) {
            Some(OutputKind::JsonEnvelope) => parse_json_envelope(&output.stdout),
            None => Ok(ToolOutput::text(output.stdout)),
        }
    }
}

impl DynamicTool {
    async fn run(
        &self,
        arguments: Value,
    ) -> Result<crate::tools::script_runner::ScriptOutput, String> {
        if self.interaction.is_some() {
            return Err("interaction tools should not reach execute(); they are intercepted in prepare_tool_call".to_string());
        }

        let script = self
            .script
            .as_ref()
            .ok_or_else(|| "dynamic tool has no script".to_string())?;

        // Validate required args
        for arg in &self.args {
            if arg.required && arguments.get(&arg.name).is_none() {
                return Err(format!("missing required argument: {}", arg.name));
            }
        }

        // Build env vars from arguments
        let mut env = Vec::new();
        if let Value::Object(map) = &arguments {
            for (key, value) in map {
                let env_name = Self::arg_to_env_name(key);
                let env_value = match value {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    Value::Bool(b) => b.to_string(),
                    Value::Array(arr) => arr
                        .iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect::<Vec<_>>()
                        .join(" "),
                    _ => value.to_string(),
                };
                env.push((env_name, env_value));
                env.push((
                    format!("{}_JSON", Self::arg_to_env_name(key)),
                    serde_json::to_string(value).unwrap_or_else(|_| "null".to_string()),
                ));
            }
        }

        let output = run_script(ScriptRequest {
            command: script.clone(),
            env,
            timeout_ms: 120_000,
        })
        .await
        .map_err(|e| e.to_string())?;

        if output.exit_code == Some(0) {
            Ok(output)
        } else {
            let code = output
                .exit_code
                .map_or_else(|| "signal".to_string(), |c| c.to_string());
            let mut msg = format!("exit code: {code}");
            if !output.stdout.is_empty() {
                msg.push_str(&format!("\nstdout:\n{}", output.stdout));
            }
            if !output.stderr.is_empty() {
                msg.push_str(&format!("\nstderr:\n{}", output.stderr));
            }
            Err(msg)
        }
    }
}

fn parse_json_envelope(stdout: &str) -> Result<ToolOutput, String> {
    let envelope: JsonEnvelope = serde_json::from_str(stdout.trim())
        .map_err(|err| format!("invalid json_envelope output: {err}"))?;
    let pane_page = envelope.pane.map(|pane| PanePage {
        source: pane.source,
        title: pane.title,
        content: pane.lines.into_iter().map(Line::from).collect(),
        visible_rows: pane.visible_rows,
        scroll: pane.scroll,
    });
    Ok(ToolOutput {
        content: envelope.content,
        pane_page,
    })
}

/// Parse all `*.yaml` files in a directory into DynamicTool instances.
/// Invalid files are warned and skipped.
pub fn load_from_dir(dir: &std::path::Path) -> Vec<DynamicTool> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut tools = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        match std::fs::read_to_string(&path) {
            Ok(raw) => {
                let raw = raw.trim_start_matches('\u{feff}');
                match serde_yaml::from_str::<DynamicTool>(raw) {
                    Ok(tool) => {
                        // Validate tool name: alphanumeric + underscores only
                        if !tool.name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                            eprintln!(
                                "bone: warning: skipping tool {}: name must be alphanumeric/underscore",
                                name
                            );
                        } else if let Err(err) = tool.validate() {
                            eprintln!("bone: warning: skipping tool {}: {err}", name);
                        } else {
                            tools.push(tool);
                        }
                    }
                    Err(err) => {
                        eprintln!("bone: warning: failed to parse tool {}: {err}", name);
                    }
                }
            }
            Err(err) => {
                eprintln!("bone: warning: failed to read {}: {err}", path.display());
            }
        }
    }
    tools
}

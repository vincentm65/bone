use async_trait::async_trait;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::tools::command_policy::CommandSafety;
use crate::tools::script_runner::{ScriptRequest, run_script, run_script_jsonl};
use crate::tools::types::{
    Tool, ToolDefinition, ToolDisplayConfig, ToolExecutionContext, ToolLiveEvent, ToolOutput,
};
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
    pub live_state: LiveStateConfig,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LiveStateConfig {
    #[serde(default)]
    pub cleanup: LiveStateCleanup,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LiveStateCleanup {
    #[default]
    ToolManaged,
    OnFinish,
}

struct LiveStateGuard {
    sender: Option<tokio::sync::mpsc::UnboundedSender<ToolLiveEvent>>,
    keys: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<(String, String)>>>,
    cleanup_on_drop: bool,
}

impl Drop for LiveStateGuard {
    fn drop(&mut self) {
        if !self.cleanup_on_drop {
            return;
        }
        let Some(sender) = self.sender.as_ref() else {
            return;
        };
        let keys = self
            .keys
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for (source, sub_key) in keys.iter() {
            let _ = sender.send(ToolLiveEvent::StateRemove {
                source: source.clone(),
                sub_key: sub_key.clone(),
            });
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OutputKind {
    JsonEnvelope,
    LineEnvelope,
    JsonlEvents,
}

#[derive(Debug, Deserialize)]
struct JsonEnvelope {
    content: String,
    #[serde(default)]
    pane: Option<PaneEnvelope>,
    #[serde(default)]
    state: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PaneEnvelope {
    source: String,
    title: String,
    #[serde(default)]
    lines: Vec<PaneLineDef>,
    #[serde(default = "default_pane_rows")]
    visible_rows: usize,
    #[serde(default)]
    scroll: usize,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum PaneLineDef {
    Text(String),
    Styled { spans: Vec<PaneSpanDef> },
}

#[derive(Debug, Deserialize)]
struct PaneSpanDef {
    text: String,
    #[serde(default)]
    fg: Option<String>,
    #[serde(default)]
    modifiers: Vec<String>,
}

impl PaneLineDef {
    fn into_line(self) -> Line<'static> {
        match self {
            PaneLineDef::Text(text) => Line::from(text),
            PaneLineDef::Styled { spans } => Line::from(
                spans
                    .into_iter()
                    .map(|span| {
                        let style = span.style();
                        Span::styled(span.text, style)
                    })
                    .collect::<Vec<_>>(),
            ),
        }
    }
}

impl PaneSpanDef {
    fn style(&self) -> Style {
        let mut style = Style::default();
        if let Some(color) = self.fg.as_deref().and_then(parse_color) {
            style = style.fg(color);
        }
        for modifier in &self.modifiers {
            match modifier.as_str() {
                "bold" => style = style.add_modifier(Modifier::BOLD),
                "dim" => style = style.add_modifier(Modifier::DIM),
                "italic" => style = style.add_modifier(Modifier::ITALIC),
                "strike" | "crossed_out" => style = style.add_modifier(Modifier::CROSSED_OUT),
                _ => {}
            }
        }
        style
    }
}

fn parse_color(name: &str) -> Option<Color> {
    match name {
        "black" => Some(Color::Black),
        "red" => Some(Color::Red),
        "green" => Some(Color::Green),
        "yellow" => Some(Color::Yellow),
        "blue" => Some(Color::Blue),
        "magenta" => Some(Color::Magenta),
        "cyan" => Some(Color::Cyan),
        "gray" | "grey" => Some(Color::Gray),
        "dark_gray" | "dark_grey" => Some(Color::DarkGray),
        "white" => Some(Color::White),
        _ => None,
    }
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
        // For JsonlEvents, use streaming execution to read stdout line-by-line.
        if self.output.as_ref().map(|o| &o.kind) == Some(&OutputKind::JsonlEvents) {
            return self.run_jsonl_events(arguments).await;
        }
        let output = self.run(arguments).await?;
        match self.output.as_ref().map(|output| &output.kind) {
            Some(OutputKind::JsonEnvelope) => parse_json_envelope(&output.stdout),
            Some(OutputKind::LineEnvelope) => parse_line_envelope(&output.stdout),
            Some(OutputKind::JsonlEvents) | None => Ok(ToolOutput::text(output.stdout)),
        }
    }

    async fn execute_output_live(
        &self,
        arguments: Value,
        events: Option<tokio::sync::mpsc::UnboundedSender<ToolLiveEvent>>,
        context: ToolExecutionContext,
    ) -> Result<ToolOutput, String> {
        if self.output.as_ref().map(|o| &o.kind) == Some(&OutputKind::JsonlEvents) {
            return self.run_jsonl_events_live(arguments, events, context).await;
        }
        self.run_with_context(arguments, Some(&context))
            .await
            .and_then(|output| match self.output.as_ref().map(|o| &o.kind) {
                Some(OutputKind::JsonEnvelope) => parse_json_envelope(&output.stdout),
                Some(OutputKind::LineEnvelope) => parse_line_envelope(&output.stdout),
                Some(OutputKind::JsonlEvents) | None => Ok(ToolOutput::text(output.stdout)),
            })
    }
}

impl DynamicTool {
    /// Run the tool script and parse JSONL events from stdout.
    async fn run_jsonl_events(&self, arguments: Value) -> Result<ToolOutput, String> {
        let script = self.script()?;
        self.validate_required_args(&arguments)?;

        let output = run_script(ScriptRequest {
            command: script.clone(),
            env: self.build_env(&arguments, None),
            timeout_ms: 300_000,
        })
        .await
        .map_err(|e| e.to_string())?;

        if output.exit_code != Some(0) {
            let code = output
                .exit_code
                .map_or_else(|| "signal".to_string(), |c| c.to_string());
            return Err(format!("exit code: {code}\n{}", output.stdout));
        }

        parse_jsonl_events(&output.stdout)
    }

    async fn run_jsonl_events_live(
        &self,
        arguments: Value,
        events: Option<tokio::sync::mpsc::UnboundedSender<ToolLiveEvent>>,
        context: ToolExecutionContext,
    ) -> Result<ToolOutput, String> {
        let script = self.script()?;
        self.validate_required_args(&arguments)?;

        let env = self.build_env(&arguments, Some(&context));
        let sender = events.clone();
        let active_sub_keys = std::sync::Arc::new(std::sync::Mutex::new(
            std::collections::HashSet::<(String, String)>::new(),
        ));
        let tracked_keys = active_sub_keys.clone();
        let _guard = LiveStateGuard {
            sender: events.clone(),
            keys: active_sub_keys.clone(),
            cleanup_on_drop: self.live_state.cleanup == LiveStateCleanup::OnFinish,
        };
        let output = match run_script_jsonl(
            ScriptRequest {
                command: script.clone(),
                env,
                timeout_ms: 300_000,
            },
            move |line| {
                if let Some(sender) = sender.as_ref()
                    && let Some(event) = parse_live_event(&line)
                {
                    match &event {
                        ToolLiveEvent::StateUpdate {
                            source, sub_key, ..
                        } => {
                            if let Ok(mut keys) = tracked_keys.lock() {
                                keys.insert((source.clone(), sub_key.clone()));
                            }
                        }
                        ToolLiveEvent::StateRemove { source, sub_key } => {
                            if let Ok(mut keys) = tracked_keys.lock() {
                                keys.remove(&(source.clone(), sub_key.clone()));
                            }
                        }
                        ToolLiveEvent::Pane(_) => {}
                    }
                    let _ = sender.send(event);
                }
            },
        )
        .await
        {
            Ok(output) => output,
            Err(err) => {
                return Err(err.to_string());
            }
        };

        if output.exit_code != Some(0) {
            let code = output
                .exit_code
                .map_or_else(|| "signal".to_string(), |c| c.to_string());
            return Err(format!("exit code: {code}\n{}", output.stdout));
        }

        parse_jsonl_events(&output.stdout)
    }

    fn script(&self) -> Result<&String, String> {
        self.script
            .as_ref()
            .ok_or_else(|| "dynamic tool has no script".to_string())
    }

    fn validate_required_args(&self, arguments: &Value) -> Result<(), String> {
        for arg in &self.args {
            if arg.required && arguments.get(&arg.name).is_none() {
                return Err(format!("missing required argument: {}", arg.name));
            }
        }
        Ok(())
    }

    fn build_env(
        &self,
        arguments: &Value,
        context: Option<&ToolExecutionContext>,
    ) -> Vec<(String, String)> {
        let mut env = Vec::new();
        if let Some(context) = context {
            env.push(("TOOL_CALL_ID".to_string(), context.call_id.clone()));
            if let Some(ref state) = context.session_state {
                env.push(("TOOL_SESSION_STATE".to_string(), state.clone()));
            }
        }
        env.push(("BONE_PID".to_string(), std::process::id().to_string()));
        if let Ok(exe) = std::env::current_exe() {
            env.push(("BONE_BIN".to_string(), exe.to_string_lossy().into_owned()));
        }
        let Value::Object(map) = arguments else {
            return env;
        };

        for (key, value) in map {
            let env_name = Self::arg_to_env_name(key);
            env.push((env_name.clone(), Self::env_value(value)));
            env.push((
                format!("{env_name}_JSON"),
                serde_json::to_string(value).unwrap_or_else(|_| "null".to_string()),
            ));
            Self::push_array_env(&mut env, &env_name, value);
        }
        env
    }

    fn env_value(value: &Value) -> String {
        match value {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Array(arr) => arr
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect::<Vec<_>>()
                .join(" "),
            _ => value.to_string(),
        }
    }

    fn push_array_env(env: &mut Vec<(String, String)>, env_name: &str, value: &Value) {
        let Value::Array(arr) = value else {
            return;
        };
        env.push((format!("{env_name}_COUNT"), arr.len().to_string()));
        for (i, item) in arr.iter().enumerate() {
            env.push((format!("{env_name}_{i}"), Self::env_value(item)));
        }
    }

    async fn run(
        &self,
        arguments: Value,
    ) -> Result<crate::tools::script_runner::ScriptOutput, String> {
        self.run_with_context(arguments, None).await
    }

    async fn run_with_context(
        &self,
        arguments: Value,
        context: Option<&ToolExecutionContext>,
    ) -> Result<crate::tools::script_runner::ScriptOutput, String> {
        if self.interaction.is_some() {
            return Err("interaction tools should not reach execute(); they are intercepted in prepare_tool_call".to_string());
        }

        let script = self.script()?;
        self.validate_required_args(&arguments)?;

        let output = run_script(ScriptRequest {
            command: script.clone(),
            env: self.build_env(&arguments, context),
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

/// Parse a JSONL line into a ToolLiveEvent.
/// Handles plain pane events (backward compatible) and state-aware events.
fn parse_live_event(line: &str) -> Option<ToolLiveEvent> {
    let event: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    if event["type"].as_str()? != "pane" {
        return None;
    }
    let state_key = event["state_key"]
        .as_str()
        .map(String::from)
        .filter(|k| !k.is_empty());
    let state = event["state"].as_str().map(String::from);
    let remove = event["remove"].as_bool().unwrap_or(false);
    let pane = pane_page_from_value(&event)?;

    match (state_key, state, remove) {
        (Some(sub_key), Some(st), false) => Some(ToolLiveEvent::StateUpdate {
            source: pane.source.clone(),
            sub_key,
            state: st,
        }),
        (Some(sub_key), _, true) => Some(ToolLiveEvent::StateRemove {
            source: pane.source.clone(),
            sub_key,
        }),
        _ => Some(ToolLiveEvent::Pane(pane)),
    }
}

fn pane_page_from_value(event: &serde_json::Value) -> Option<PanePage> {
    let pane: PaneEnvelope = serde_json::from_value(event.get("pane")?.clone()).ok()?;
    Some(PanePage {
        source: pane.source,
        title: pane.title,
        content: pane.lines.into_iter().map(PaneLineDef::into_line).collect(),
        visible_rows: pane.visible_rows,
        scroll: pane.scroll,
    })
}

fn parse_line_envelope(stdout: &str) -> Result<ToolOutput, String> {
    let mut content = String::new();
    let mut pane_source = String::new();
    let mut pane_title = String::new();
    let mut pane_lines: Vec<String> = Vec::new();
    let mut pane_visible_rows = DEFAULT_PANE_ROWS;
    let mut pane_scroll: usize = 0;
    let mut has_pane = false;

    enum Section {
        Content,
        PaneMeta,
        PaneLines,
    }
    let mut section = Section::Content;

    for line in stdout.lines() {
        match line {
            "@@content@@" => section = Section::Content,
            "@@pane@@" => {
                has_pane = true;
                section = Section::PaneMeta;
            }
            "@@lines@@" => section = Section::PaneLines,
            "@@end@@" => section = Section::Content,
            _ => match section {
                Section::Content => {
                    if !content.is_empty() {
                        content.push('\n');
                    }
                    content.push_str(line);
                }
                Section::PaneMeta => {
                    if let Some(value) = line.strip_prefix("source: ") {
                        pane_source = value.to_string();
                    } else if let Some(value) = line.strip_prefix("title: ") {
                        pane_title = value.to_string();
                    } else if let Some(value) = line.strip_prefix("visible_rows: ") {
                        pane_visible_rows = value.parse().unwrap_or(DEFAULT_PANE_ROWS);
                    } else if let Some(value) = line.strip_prefix("scroll: ") {
                        pane_scroll = value.parse().unwrap_or(0);
                    }
                }
                Section::PaneLines => {
                    pane_lines.push(line.to_string());
                }
            },
        }
    }

    let pane_page = if has_pane {
        Some(PanePage {
            source: pane_source,
            title: pane_title,
            content: pane_lines.into_iter().map(Line::from).collect(),
            visible_rows: pane_visible_rows,
            scroll: pane_scroll,
        })
    } else {
        None
    };

    Ok(ToolOutput {
        content,
        pane_page,
        state: None,
    })
}

fn parse_json_envelope(stdout: &str) -> Result<ToolOutput, String> {
    let envelope: JsonEnvelope = serde_json::from_str(stdout.trim())
        .map_err(|err| format!("invalid json_envelope output: {err}"))?;
    let pane_page = envelope.pane.map(|pane| PanePage {
        source: pane.source,
        title: pane.title,
        content: pane.lines.into_iter().map(PaneLineDef::into_line).collect(),
        visible_rows: pane.visible_rows,
        scroll: pane.scroll,
    });
    Ok(ToolOutput {
        content: envelope.content,
        pane_page,
        state: envelope.state,
    })
}
fn parse_jsonl_events(stdout: &str) -> Result<ToolOutput, String> {
    let mut content = String::new();
    let mut explicit_pane_page: Option<PanePage> = None;

    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let event: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("bone: warning: skipping non-JSON line in tool output: {e}");
                continue;
            }
        };
        if event["type"].as_str() == Some("pane") {
            if event.get("state_key").is_none() {
                explicit_pane_page = pane_page_from_value(&event);
            }
            continue;
        }
        if event["type"].as_str() == Some("text_delta") {
            if let Some(text) = event["text"].as_str() {
                content.push_str(text);
            }
        } else if event["type"].as_str() == Some("finished") && content.is_empty() {
            content = event["content"].as_str().unwrap_or("").to_string();
        } else if event["type"].as_str() == Some("failed") && content.is_empty() {
            let msg = event["message"].as_str().unwrap_or("unknown error");
            content = format!("Tool failed: {msg}");
        }
    }

    if content.is_empty() && !stdout.is_empty() {
        content = stdout.to_string();
    }

    Ok(ToolOutput {
        content,
        pane_page: explicit_pane_page,
        state: None,
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

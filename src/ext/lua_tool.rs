//! `LuaTool` — a tool registered from Lua via `bone.register_tool()`.
//!
//! Implements the `Tool` trait so it can be registered in `ToolRegistry`
//! alongside native and dynamic tools.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use mlua::{Lua, LuaSerdeExt};
use serde_json::Value;

use crate::tools::types::{Tool, ToolDefinition, ToolDisplayConfig, ToolOutput};
use crate::ui::pane_page::PanePage;

use super::ctx::{self, CtxConfig, SharedState};
use crate::tools::command_policy::CommandSafety;

pub struct LuaTool {
    name: String,
    description: String,
    parameters: Value,
    display: ToolDisplayConfig,
    lua: Arc<Mutex<Lua>>,
    registry_key: Arc<mlua::RegistryKey>,
    cwd: String,
    config_dir: String,
    shared_state: SharedState,
    safety: CommandSafety,
}

impl LuaTool {
    /// Build a `LuaTool` from a validated `_tools` entry table.
    ///
    /// The caller must hold the Lua lock while calling this.
    pub fn from_entry(
        lua: &Lua,
        entry: &mlua::Table,
        lua_arc: Arc<Mutex<Lua>>,
        cwd: String,
        config_dir: String,
        shared_state: SharedState,
    ) -> Result<Self, String> {
        let name: String = entry
            .get("name")
            .map_err(|e| format!("tool entry missing name: {e}"))?;
        let description: String = entry
            .get("description")
            .map_err(|e| format!("tool '{}' missing description: {e}", name))?;

        // Convert parameters table to serde_json::Value.
        let params_val: mlua::Value = entry
            .get("parameters")
            .map_err(|e| format!("tool '{}' missing parameters: {e}", name))?;
        let parameters: Value = lua
            .from_value(params_val)
            .map_err(|e| format!("tool '{}' invalid parameters schema: {e}", name))?;

        // Extract display config if present.
        let display = match entry.get::<mlua::Value>("display") {
            Ok(mlua::Value::Table(t)) => {
                let args: Vec<String> = t
                    .get::<Option<mlua::Table>>("args")
                    .ok()
                    .flatten()
                    .map(|tbl| {
                        tbl.sequence_values::<String>()
                            .filter_map(|v| v.ok())
                            .collect()
                    })
                    .unwrap_or_default();

                let template: Option<String> = t
                    .get::<Option<String>>("template")
                    .ok()
                    .flatten();

                let show: Option<bool> = t.get::<Option<bool>>("show").ok().flatten();
                let show_result: Option<bool> =
                    t.get::<Option<bool>>("show_result").ok().flatten();

                ToolDisplayConfig {
                    args,
                    template,
                    show,
                    show_result,
                }
            }
            _ => ToolDisplayConfig::default(),
        };

        // Extract safety classification.
        let safety = match entry.get::<String>("safety") {
            Ok(s) => match s.as_str() {
                "read_only" | "safe" => CommandSafety::ReadOnly,
                _ => CommandSafety::Danger,
            },
            _ => CommandSafety::Danger,
        };
        // Take ownership of the execute function via the registry.
        let execute_fn: mlua::Value = entry
            .get("execute")
            .map_err(|e| format!("tool '{}' missing execute: {e}", name))?;
        let registry_key = lua
            .create_registry_value(execute_fn)
            .map_err(|e| format!("tool '{}' failed to store execute fn: {e}", name))?;

        Ok(Self {
            name,
            description,
            parameters,
            display,
            lua: lua_arc,
            registry_key: Arc::new(registry_key),
            cwd,
            safety,
            config_dir,
            shared_state,
        })
    }

    pub fn display(&self) -> &ToolDisplayConfig {
        &self.display
    }
    pub fn safety(&self) -> CommandSafety {
        self.safety
    }
}

#[async_trait]
impl Tool for LuaTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.clone(),
            description: self.description.clone(),
            input_schema: self.parameters.clone(),
        }
    }

    async fn execute(&self, arguments: Value) -> Result<String, String> {
        let lua = self.lua.lock().unwrap_or_else(|e| e.into_inner());

        let execute_fn: mlua::Value = lua
            .registry_value(&*self.registry_key)
            .map_err(|e| format!("lua tool '{}': execute function lost: {e}", self.name))?;
        let execute_fn = match execute_fn {
            mlua::Value::Function(f) => f,
            _ => return Err(format!("lua tool '{}': execute is not a function", self.name)),
        };

        let args_lua = lua
            .to_value(&arguments)
            .map_err(|e| format!("lua tool '{}': failed to convert arguments: {e}", self.name))?;

        // execute (non-live)
        let ctx_cfg = CtxConfig {
            cwd: self.cwd.clone(),
            config_dir: self.config_dir.clone(),
            shared_state: self.shared_state.clone(),
            pane_sender: None,
            call_id: None,
        };
        let ctx_table = ctx::create_ctx_table(&lua, &ctx_cfg)
                        .map_err(|e| format!("lua tool '{}': failed to create ctx: {e}", self.name))?;

        let result = execute_fn.call::<mlua::Value>((args_lua, ctx_table));

        match result {
            Ok(mlua::Value::String(s)) => Ok(s.to_str().map(|s| s.to_string()).unwrap_or_else(|e| format!("(lua string error: {e})"))),
            Ok(mlua::Value::Nil) => Ok(String::new()),
            Ok(v) => Ok(format!("{v:?}")),
            Err(e) => Err(format!("lua tool '{}': {e}", self.name)),
        }
    }

    async fn execute_output(&self, arguments: Value) -> Result<ToolOutput, String> {
        let lua = self.lua.lock().unwrap_or_else(|e| e.into_inner());

        let execute_fn: mlua::Value = lua
            .registry_value(&*self.registry_key)
            .map_err(|e| format!("lua tool '{}': execute function lost: {e}", self.name))?;
        let execute_fn = match execute_fn {
            mlua::Value::Function(f) => f,
            _ => return Err(format!("lua tool '{}': execute is not a function", self.name)),
        };

        let args_lua = lua
            .to_value(&arguments)
            .map_err(|e| format!("lua tool '{}': failed to convert arguments: {e}", self.name))?;

        let ctx_cfg = CtxConfig {
            cwd: self.cwd.clone(),
            config_dir: self.config_dir.clone(),
            shared_state: self.shared_state.clone(),
            pane_sender: None,
            call_id: None,
        };
        let ctx_table = ctx::create_ctx_table(&lua, &ctx_cfg)
                        .map_err(|e| format!("lua tool '{}': failed to create ctx: {}", self.name, e))?;

        let result = execute_fn.call::<mlua::Value>((args_lua, ctx_table));

        let text = match result {
            Ok(mlua::Value::String(s)) => s.to_str().map(|s| s.to_string()).unwrap_or_else(|e| format!("(lua string error: {e})")),
            Ok(mlua::Value::Nil) => String::new(),
            Ok(v) => format!("{v:?}"),
            Err(e) => return Err(format!("lua tool '{}': {e}", self.name)),
        };

        parse_tool_output(&text)
    }

    async fn execute_output_live(
        &self,
        arguments: Value,
        events: Option<tokio::sync::mpsc::UnboundedSender<crate::tools::types::ToolLiveEvent>>,
        _context: crate::tools::types::ToolExecutionContext,
    ) -> Result<ToolOutput, String> {
        // Clone handles for move into spawn_blocking.
        let lua_arc = self.lua.clone();
        let registry_key = self.registry_key.clone();
        let name = self.name.clone();
        let cwd = self.cwd.clone();
        let config_dir = self.config_dir.clone();
        let shared_state = self.shared_state.clone();

        tokio::task::spawn_blocking(move || {
            let lua = lua_arc.lock().unwrap_or_else(|e| e.into_inner());

            let execute_fn: mlua::Value = lua
                .registry_value(&*registry_key)
                .map_err(|e| format!("lua tool '{name}': execute function lost: {e}"))?;
            let execute_fn = match execute_fn {
                mlua::Value::Function(f) => f,
                _ => return Err(format!("lua tool '{name}': execute is not a function")),
            };

            let args_lua = lua
                .to_value(&arguments)
                .map_err(|e| format!("lua tool '{name}': failed to convert arguments: {e}"))?;

            let ctx_cfg = CtxConfig {
                cwd,
                config_dir,
                shared_state,
                pane_sender: events,
                call_id: Some(_context.call_id),
            };
            let ctx_table = ctx::create_ctx_table(&lua, &ctx_cfg)
                .map_err(|e| format!("lua tool '{name}': failed to create ctx: {e}"))?;

            let result = execute_fn.call::<mlua::Value>((args_lua, ctx_table));

            let text = match result {
                Ok(mlua::Value::String(s)) => s.to_str().map(|s| s.to_string()).unwrap_or_else(|e| format!("(lua string error: {e})")),
                Ok(mlua::Value::Nil) => String::new(),
                Ok(v) => format!("{v:?}"),
                Err(e) => return Err(format!("lua tool '{name}': {e}")),
            };

            parse_tool_output(&text)
        })
        .await
        .map_err(|e| format!("lua tool '{}': spawn_blocking panicked: {e}", self.name))?
    }
}

/// Parse tool output text — tries JSON envelope, falls back to plain text.
fn parse_tool_output(text: &str) -> Result<ToolOutput, String> {
    match serde_json::from_str::<serde_json::Value>(text.trim()) {
        Ok(obj) if obj.is_object() => {
            let map = obj.as_object().unwrap();
            let content = map.get("content").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let state = map.get("state").and_then(|v| v.as_str()).map(|s| s.to_string());
            let pane_page = map.get("pane").and_then(|pane_val| {
                let pane = pane_val.as_object()?;
                let source = pane.get("source").and_then(|v| v.as_str())?.to_string();
                let title = pane.get("title").and_then(|v| v.as_str())?.to_string();
                let visible_rows = pane.get("visible_rows").and_then(|v| v.as_u64()).unwrap_or(8) as usize;
                let scroll = pane.get("scroll").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let lines: Vec<ratatui::text::Line<'static>> = pane
                    .get("lines")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|line_val| {
                                if let Some(text) = line_val.as_str() {
                                    Some(ratatui::text::Line::from(text.to_string()))
                                } else if let Some(styled) = line_val.as_object() {
                                    let spans: Vec<ratatui::text::Span<'static>> = styled
                                        .get("spans")
                                        .and_then(|v| v.as_array())
                                        .map(|spans_arr| {
                                            spans_arr.iter()
                                                .filter_map(|span_val| {
                                                    let span_obj = span_val.as_object()?;
                                                    let text = span_obj.get("text").and_then(|v| v.as_str())?.to_string();
                                                    let style = span_obj_to_style(span_obj);
                                                    Some(ratatui::text::Span::styled(text, style))
                                                })
                                                .collect()
                                        })
                                        .unwrap_or_default();
                                    Some(ratatui::text::Line::from(spans))
                                } else {
                                    None
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                Some(PanePage {
                    source,
                    title,
                    content: lines,
                    visible_rows,
                    scroll,
                })
            });
            Ok(ToolOutput {
                content,
                pane_page,
                state,
            })
        }
        _ => Ok(ToolOutput::text(text.to_string())),
    }
}

fn span_obj_to_style(obj: &serde_json::Map<String, serde_json::Value>) -> ratatui::style::Style {
    use ratatui::style::{Color, Modifier, Style};
    let mut style = Style::default();
    if let Some(color) = obj.get("fg").and_then(|v| v.as_str()) {
        let color = match color {
            "black" => Color::Black,
            "red" => Color::Red,
            "green" => Color::Green,
            "yellow" => Color::Yellow,
            "blue" => Color::Blue,
            "magenta" => Color::Magenta,
            "cyan" => Color::Cyan,
            "gray" | "grey" => Color::Gray,
            "dark_gray" | "dark_grey" => Color::DarkGray,
            "white" => Color::White,
            _ => return style,
        };
        style = style.fg(color);
    }
    if let Some(modifiers) = obj.get("modifiers").and_then(|v| v.as_array()) {
        for mod_val in modifiers {
            if let Some(m) = mod_val.as_str() {
                match m {
                    "bold" => style = style.add_modifier(Modifier::BOLD),
                    "dim" => style = style.add_modifier(Modifier::DIM),
                    "italic" => style = style.add_modifier(Modifier::ITALIC),
                    "strike" | "crossed_out" => style = style.add_modifier(Modifier::CROSSED_OUT),
                    _ => {}
                }
            }
        }
    }
    style
}

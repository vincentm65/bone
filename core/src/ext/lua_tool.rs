//! `LuaTool` — a tool registered from Lua via `bone.tool.register()`.
//!
//! Implements the `Tool` trait so it can be registered in `ToolRegistry`
//! alongside native and dynamic tools.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use mlua::{Lua, LuaSerdeExt};
use serde_json::Value;

use crate::pane_content::PaneContent;
use crate::tools::types::{
    Tool, ToolDefinition, ToolDisplayConfig, ToolExecutionContext, ToolOutput,
};

use super::ctx::{self, CtxConfig, SharedState};
use crate::tools::command_policy::CommandSafety;

pub struct LuaTool {
    name: String,
    description: String,
    parameters: Value,
    display: ToolDisplayConfig,
    lua: Arc<Mutex<Lua>>,
    registry_key: Arc<mlua::RegistryKey>,
    config_dir: String,
    shared_state: SharedState,
    safety: CommandSafety,
    /// Host-held state key when the tool declares `stateful = true`. The host
    /// serializes batched calls to this tool and threads the prior result's
    /// `state` back in before each call. `None` for ordinary stateless tools.
    state_key: Option<String>,
    ui: super::api_ui::SharedUi,
}

impl LuaTool {
    /// Build a `LuaTool` from a validated `_tools` entry table.
    ///
    /// The caller must hold the Lua lock while calling this.
    pub fn from_entry(
        lua: &Lua,
        entry: &mlua::Table,
        lua_arc: Arc<Mutex<Lua>>,
        config_dir: String,
        shared_state: SharedState,
        ui: super::api_ui::SharedUi,
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
        let mut parameters: Value = lua
            .from_value(params_val)
            .map_err(|e| format!("tool '{}' invalid parameters schema: {e}", name))?;
        normalize_json_schema(&mut parameters);

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

                let template: Option<String> = t.get::<Option<String>>("template").ok().flatten();

                let show: Option<bool> = t.get::<Option<bool>>("show").ok().flatten();
                let show_result: Option<bool> = t.get::<Option<bool>>("show_result").ok().flatten();
                let eager: Option<bool> = t.get::<Option<bool>>("eager").ok().flatten();

                ToolDisplayConfig {
                    args,
                    template,
                    show,
                    show_result,
                    eager,
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
        // Host-held state opt-in: `stateful = true` makes the host serialize
        // batched calls and feed the prior result's `state` back in. An explicit
        // `state_key` overrides the default (the tool name), which is also the
        // pane `source` the state is reconciled against.
        let stateful: bool = entry.get("stateful").unwrap_or(false);
        let state_key: Option<String> = if stateful {
            Some(
                entry
                    .get::<Option<String>>("state_key")
                    .ok()
                    .flatten()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| name.clone()),
            )
        } else {
            None
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
            safety,
            state_key,
            config_dir,
            shared_state,
            ui,
        })
    }

    pub fn display(&self) -> &ToolDisplayConfig {
        &self.display
    }
    pub fn safety(&self) -> CommandSafety {
        self.safety
    }
    /// Host-held state key, or `None` when the tool is stateless.
    pub fn state_key(&self) -> Option<&str> {
        self.state_key.as_deref()
    }

    /// Run the tool's Lua execute function synchronously on the current thread.
    ///
    /// The project `Arc<Mutex<Lua>>` is held only while extracting the execute
    /// function, converting arguments, and building the ctx table. It is
    /// released before calling into Lua: `std::sync::Mutex` is not reentrant,
    /// and a nested LuaTool invocation (via `ctx.tools.call`) runs inline on
    /// this same thread and must be able to re-acquire it. Cross-thread access
    /// to the VM during the call is still serialized by mlua's internal
    /// reentrant VM mutex (`send` feature).
    #[allow(clippy::too_many_arguments)]
    fn run_execute(
        lua_arc: &Arc<Mutex<Lua>>,
        registry_key: &mlua::RegistryKey,
        name: &str,
        arguments: &Value,
        config_dir: String,
        shared_state: SharedState,
        events: Option<tokio::sync::mpsc::UnboundedSender<crate::pane_content::KeyRequest>>,
        ui: super::api_ui::SharedUi,
        context: &ToolExecutionContext,
    ) -> Result<ToolOutput, String> {
        let lua = lua_arc.lock().unwrap_or_else(|e| e.into_inner());

        let execute_fn: mlua::Value = lua
            .registry_value(registry_key)
            .map_err(|e| format!("lua tool '{name}': execute function lost: {e}"))?;
        let execute_fn = match execute_fn {
            mlua::Value::Function(f) => f,
            _ => return Err(format!("lua tool '{name}': execute is not a function")),
        };

        let args_lua = lua
            .to_value(arguments)
            .map_err(|e| format!("lua tool '{name}': failed to convert arguments: {e}"))?;

        // Prefer the session ToolHandler's map (survives ReloadExtensions and
        // is conversation-scoped). Fall back to the Arc captured at collect
        // time for bare execute paths that have no handler.
        let session_state = context
            .tool_handler
            .as_ref()
            .map(|h| h.shared_state.clone())
            .unwrap_or(shared_state);
        let mut ctx_cfg = CtxConfig::new(config_dir, session_state);
        if let Some(working_dir) = &context.working_dir {
            ctx_cfg.cwd = working_dir.to_string_lossy().into_owned();
        }
        // App-derived fields (session/provider/model/usage/history/approval_mode)
        // so tools see the same `ctx` as slash commands. `None` for non-live
        // calls, which keeps the previous all-default behavior. `apply_to` also
        // re-points `shared_state` at the session map.
        if let Some(state) = &context.app_state {
            state.apply_to(&mut ctx_cfg);
        }
        ctx_cfg.key_sender = events;
        ctx_cfg.ui = Some(ui.clone());
        ctx_cfg.call_id = Some(context.call_id.clone());
        // tool_handler comes from the per-call context (may differ from the
        // snapshot's handler for nested delegation), so set it after apply_to.
        if let Some(handler) = context.tool_handler.clone() {
            ctx_cfg.shared_state = handler.shared_state.clone();
            ctx_cfg.tool_handler = Some(handler);
        }
        ctx_cfg.tool_call_depth = context.tool_call_depth;
        ctx_cfg.agent_depth = context.agent_depth;
        ctx_cfg.cancelled = context.cancelled.clone();
        ctx_cfg.approval_gate = context.approval_gate.clone();
        let ctx_table = ctx::create_ctx_table(&lua, &ctx_cfg)
            .map_err(|e| format!("lua tool '{name}': failed to create ctx: {e}"))?;

        // Release the project mutex before calling into Lua (see doc comment).
        drop(lua);

        let result = execute_fn.call::<mlua::Value>((args_lua, ctx_table));
        drop(execute_fn);
        let lua = lua_arc.lock().unwrap_or_else(|e| e.into_inner());
        let _ = lua.gc_collect();
        drop(lua);

        let text = match result {
            Ok(mlua::Value::String(s)) => s
                .to_str()
                .map(|s| s.to_string())
                .unwrap_or_else(|e| format!("(lua string error: {e})")),
            Ok(mlua::Value::Nil) => String::new(),
            Ok(v) => format!("{v:?}"),
            Err(e) => return Err(format!("lua tool '{name}': {e}")),
        };

        let output = parse_tool_output(&text)?;

        // A tool-result `pane` envelope (host-stateful tools like `task_list`)
        // renders through the same shared `UiState` handle the TUI drains as
        // `ctx.ui.pane` — the only live-pane transport since the channel was
        // retired. Without this push the pane is parsed into `pane_page` (and
        // used for host state) but never reaches the screen. Skip for subagents
        // (depth > 0): their panes must not leak into the parent TUI.
        if context.agent_depth == 0
            && let Some(pane) = &output.pane_page
        {
            let diff = crate::runtime::view::view_diff_from_pane_content(pane.clone());
            super::api_ui::lock_shared(&ui).apply(diff);
        }

        Ok(output)
    }
}

/// Remove the legacy boolean `required` shim; delete once all seeded/catalog tools are migrated.
fn normalize_json_schema(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if matches!(map.get("required"), Some(Value::Bool(_))) {
                map.remove("required");
            }
            for child in map.values_mut() {
                normalize_json_schema(child);
            }
        }
        Value::Array(items) => {
            for child in items {
                normalize_json_schema(child);
            }
        }
        _ => {}
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
        let output = self
            .execute_output_live(arguments, None, ToolExecutionContext::default())
            .await?;
        Ok(output.content)
    }

    async fn execute_output(&self, arguments: Value) -> Result<ToolOutput, String> {
        self.execute_output_live(arguments, None, ToolExecutionContext::default())
            .await
    }

    async fn execute_output_live(
        &self,
        arguments: Value,
        events: Option<tokio::sync::mpsc::UnboundedSender<crate::pane_content::KeyRequest>>,
        context: crate::tools::types::ToolExecutionContext,
    ) -> Result<ToolOutput, String> {
        if context.tool_call_depth > 0 {
            // Nested invocation via ctx.tools.call: we are already on the
            // thread that is executing Lua (inside Function::call), which
            // holds mlua's internal VM mutex. That mutex is reentrant only
            // for the *same* thread, so hopping to another thread here would
            // deadlock: the new thread would block on the VM mutex while this
            // thread blocks waiting for its result. Execute inline instead —
            // same-thread re-entry into the VM is sound (Lua supports
            // recursive lua_pcall from callbacks). The caller is already
            // inside block_in_place/block_on, so blocking here is fine.
            return Self::run_execute(
                &self.lua,
                &self.registry_key,
                &self.name,
                &arguments,
                self.config_dir.clone(),
                self.shared_state.clone(),
                events,
                self.ui.clone(),
                &context,
            );
        }

        // Top-level call: run blocking Lua execution off the async workers.
        let lua_arc = self.lua.clone();
        let registry_key = self.registry_key.clone();
        let name = self.name.clone();
        let config_dir = self.config_dir.clone();
        let shared_state = self.shared_state.clone();
        let ui = self.ui.clone();

        tokio::task::spawn_blocking(move || {
            Self::run_execute(
                &lua_arc,
                &registry_key,
                &name,
                &arguments,
                config_dir,
                shared_state,
                events,
                ui,
                &context,
            )
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
            if !["content", "state", "pane", "images"]
                .iter()
                .any(|key| map.contains_key(*key))
            {
                return Ok(ToolOutput::text(text.to_string()));
            }
            let content = map
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let state = map
                .get("state")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let pane_page = map
                .get("pane")
                .and_then(|pane_val| PaneContent::from_json(pane_val).ok());
            // Optional `images`: an array of `{ media_type, data }` (base64),
            // relayed to vision-capable models. Malformed entries are skipped.
            let images = map
                .get("images")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|img| {
                            let media_type = img.get("media_type")?.as_str()?.to_string();
                            let data = img.get("data")?.as_str()?.to_string();
                            Some(crate::llm::ImageData { media_type, data })
                        })
                        .collect()
                })
                .unwrap_or_default();
            Ok(ToolOutput {
                content,
                images,
                pane_page,
                state,
            })
        }
        _ => Ok(ToolOutput::text(text.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_tool_output;

    #[test]
    fn preserves_plain_json_object_as_content() {
        let text = r#"{"cancelled":false,"answers":[{"value":"Blue"}]}"#;
        let output = parse_tool_output(text).unwrap();

        assert_eq!(output.content, text);
        assert!(output.images.is_empty());
        assert!(output.pane_page.is_none());
        assert!(output.state.is_none());
    }

    #[test]
    fn still_parses_tool_output_envelopes() {
        let output = parse_tool_output(r#"{"content":"done","state":"saved"}"#).unwrap();

        assert_eq!(output.content, "done");
        assert_eq!(output.state.as_deref(), Some("saved"));
    }
}

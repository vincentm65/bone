//! Extension system types.

use std::sync::{Arc, Mutex};

use mlua::{Lua, LuaSerdeExt};

use super::snapshots::{LuaConfigSnapshot, LuaKeymapSnapshot, LuaThemeSnapshot};
use crate::tools::ToolCall;

/// Options controlling the Lua boot context.
///
/// Exposed to Lua as `bone.agent_depth` and `bone.headless` before
/// `init.lua` and tool/command files execute, so scripts can adapt
/// (e.g. the subagent tool refuses to register inside sub-agent VMs).
#[derive(Clone, Copy, Default)]
pub struct BootOptions {
    /// Nesting depth of the agent owning this VM (0 = top-level).
    pub agent_depth: usize,
    /// True when running without the TUI (CLI/headless agent). Background
    /// job auto-injection is unavailable, so tools must block for results.
    pub headless: bool,
}

/// Result of dispatching an event through all Lua handlers.
#[derive(Debug, Clone)]
pub enum EventDispatchResult {
    /// No handler blocked; continue normally.
    Continue,
    /// A handler requested blocking.
    Blocked { reason: String },
}

/// Generic action returned by a Lua command or hook.
#[derive(Debug, Clone, Default)]
pub struct LuaReturnAction {
    /// When set, replace the active conversation transcript with these messages.
    /// Used by compaction: swaps the transcript but keeps the rendered scrollback
    /// and the current conversation id.
    pub conversation_replace: Option<Vec<crate::llm::ChatMessage>>,
    /// When set, load a past conversation as the active chat: clears the current
    /// scrollback/transcript and resumes the given conversation in place.
    pub conversation_load: Option<ConversationLoad>,
}

/// Payload for the `conversation.load` action (`/history`).
#[derive(Debug, Clone)]
pub struct ConversationLoad {
    pub messages: Vec<crate::llm::ChatMessage>,
    /// Conversation id to resume; future messages append here.
    pub conversation_id: Option<i64>,
}

/// Normalized result from a Lua command handler.
#[derive(Debug, Clone)]
pub struct LuaCommandReturn {
    pub output: String,
    pub submit: bool,
    pub action: Option<LuaReturnAction>,
}

/// Owning manager for the Lua VM and all registered extension data.
///
/// `Clone` is cheap: the Lua VM is shared via `Arc<Mutex<Lua>>` and the
/// remaining fields are small snapshots/vecs. Cloning lets callers hand an
/// owned manager to `spawn_blocking` (e.g. to run `before_turn` off the UI
/// thread) without giving up their own copy.
#[derive(Clone)]
pub struct ExtensionManager {
    /// The Lua state, shared so LuaTool can also hold a reference.
    lua: Arc<Mutex<Lua>>,
    /// `true` when the Lua engine booted successfully.
    engine_ok: bool,
    /// `true` when `init.lua` was loaded without errors.
    loaded: bool,
    /// Commands registered via `bone.register_command()` during init.lua.
    commands: Vec<super::ops_commands::RegisteredLuaCommand>,
    /// Snapshot of `bone.config` captured after init.lua.
    config_snapshot: LuaConfigSnapshot,
    /// Snapshot of `bone.theme` captured after init.lua.
    theme_snapshot: LuaThemeSnapshot,
    /// Snapshot of `bone.keymap` captured after init.lua.
    keymap_snapshot: LuaKeymapSnapshot,
    /// Names of sub-agents registered via `bone.register_subagent()`.
    subagents: Vec<String>,
}

impl ExtensionManager {
    /// Wrap a pre-created `Arc<Mutex<Lua>>`.
    pub(crate) fn from_arc(
        lua: Arc<Mutex<Lua>>,
        engine_ok: bool,
        loaded: bool,
        commands: Vec<super::ops_commands::RegisteredLuaCommand>,
        config_snapshot: LuaConfigSnapshot,
        theme_snapshot: LuaThemeSnapshot,
        keymap_snapshot: LuaKeymapSnapshot,
        subagents: Vec<String>,
    ) -> Self {
        Self {
            lua,
            engine_ok,
            loaded,
            commands,
            config_snapshot,
            theme_snapshot,
            keymap_snapshot,
            subagents,
        }
    }

    /// Returns `true` when the Lua runtime booted and `init.lua` (if present)
    /// executed without errors.
    /// Returns `true` when the Lua engine booted successfully
    /// (regardless of whether `init.lua` exists or ran without errors).
    pub fn is_available(&self) -> bool {
        self.engine_ok
    }

    /// Clone the underlying Lua state handle.
    pub(crate) fn lua_handle(&self) -> Arc<Mutex<Lua>> {
        self.lua.clone()
    }

    /// Clone the underlying Lua state handle (public for testing).
    pub fn lua_arc(&self) -> Arc<Mutex<Lua>> {
        self.lua_handle()
    }

    /// Get registered Lua commands.
    pub fn commands(&self) -> &[super::ops_commands::RegisteredLuaCommand] {
        &self.commands
    }

    /// Get the Lua config snapshot captured at boot.
    pub fn config_snapshot(&self) -> &LuaConfigSnapshot {
        &self.config_snapshot
    }

    /// Get the Lua theme snapshot captured at boot.
    pub fn theme_snapshot(&self) -> &LuaThemeSnapshot {
        &self.theme_snapshot
    }

    /// Get the Lua keymap snapshot captured at boot.
    pub fn keymap_snapshot(&self) -> &LuaKeymapSnapshot {
        &self.keymap_snapshot
    }

    /// Names of sub-agents registered at boot (empty when none).
    pub fn subagent_names(&self) -> &[String] {
        &self.subagents
    }

    /// Dispatch a simple (non-blockable) event with a JSON-serializable
    /// payload. Used for `session_start`, `session_end`, `message`,
    /// `mode_change`.
    pub fn dispatch_simple(&self, name: &str, payload: serde_json::Value) {
        if !self.loaded {
            return;
        }
        let lua = match guard_with_bone(&self.lua) {
            Some(g) => g,
            None => return,
        };
        dispatch_event_inner(&lua, name, payload, false);
    }

    /// Dispatch a `tool_call` event. Returns `Blocked` if any handler
    /// returned `{ block = true, reason = "..." }`.
    pub fn dispatch_tool_call(
        &self,
        tool_name: &str,
        call_id: &str,
        arguments: &serde_json::Value,
        safety: &str,
    ) -> EventDispatchResult {
        if !self.loaded {
            return EventDispatchResult::Continue;
        }
        let lua = match guard_with_bone(&self.lua) {
            Some(g) => g,
            None => return EventDispatchResult::Continue,
        };
        let payload = serde_json::json!({
            "name": tool_name,
            "call_id": call_id,
            "arguments": arguments,
            "safety": safety,
        });
        dispatch_event_inner(&lua, "tool_call", payload, true)
    }

    /// Dispatch a `tool_result` event (non-blockable).
    pub fn dispatch_tool_result(&self, tool_name: &str, call_id: &str, is_error: bool) {
        if !self.loaded {
            return;
        }
        let lua = match guard_with_bone(&self.lua) {
            Some(g) => g,
            None => return,
        };
        let payload = serde_json::json!({
            "name": tool_name,
            "call_id": call_id,
            "is_error": is_error,
        });
        dispatch_event_inner(&lua, "tool_result", payload, false);
    }

    /// Dispatch the `before_turn` event with a full ctx (including conversation
    /// history). Collects and returns actions from all handlers in registration
    /// order.
    pub(crate) fn dispatch_before_turn(
        &self,
        ctx_cfg: &crate::ext::ctx::CtxConfig,
    ) -> Vec<LuaReturnAction> {
        if !self.loaded {
            return Vec::new();
        }
        let lua = match guard_with_bone(&self.lua) {
            Some(g) => g,
            None => return Vec::new(),
        };

        let bone = match lua.globals().get::<Option<mlua::Table>>("bone") {
            Ok(Some(t)) => t,
            _ => return Vec::new(),
        };

        let handlers = match bone.get::<Option<mlua::Table>>("_handlers") {
            Ok(Some(t)) => t,
            _ => return Vec::new(),
        };

        let event_handlers = match handlers.get::<Option<mlua::Table>>("before_turn") {
            Ok(Some(t)) => t,
            _ => return Vec::new(),
        };

        let ctx_table = match crate::ext::ctx::create_ctx_table(&lua, ctx_cfg) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("bone-lua warn: before_turn ctx creation failed: {e}");
                return Vec::new();
            }
        };

        // Event payload: minimal (the handler has ctx for everything).
        let event_table = match lua.create_table() {
            Ok(t) => t,
            Err(e) => {
                eprintln!("bone-lua warn: before_turn event table failed: {e}");
                return Vec::new();
            }
        };

        let mut actions = Vec::new();
        for handler in event_handlers.sequence_values::<mlua::Function>() {
            let handler = match handler {
                Ok(h) => h,
                Err(_) => continue,
            };

            match handler.call::<mlua::Value>((event_table.clone(), ctx_table.clone())) {
                Ok(mlua::Value::Table(ret)) => {
                    if let Some(action) = parse_lua_return_action(&ret) {
                        actions.push(action);
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    eprintln!("bone-lua warn: before_turn handler error: {e}");
                }
            }
        }

        actions
    }
}

/// Result of booting the Lua extension system.
pub struct BootResult {
    /// The extension manager (keeps the Lua VM alive).
    pub manager: ExtensionManager,
    /// Tools registered via `bone.register_tool()` during init.lua.
    pub tools: Vec<super::lua_tool::LuaTool>,
}

/// Fully booted tool system: extension manager + configured tool handler.
pub struct BootedTools {
    /// The extension manager (keeps the Lua VM alive).
    pub manager: ExtensionManager,
    /// The configured tool handler.
    pub tools: crate::tools::registry::ToolHandler,
}

/// Normalize a value returned by a Lua command handler.
///
/// String returns are prompts to submit into the agent loop. Table returns can
/// override that with `submit = false` for display-only command output.
pub fn parse_lua_command_return(value: mlua::Value) -> Option<LuaCommandReturn> {
    match value {
        mlua::Value::String(s) => {
            let output = s.to_str().map(|s| s.to_string()).unwrap_or_default();
            if output.is_empty() {
                None
            } else {
                Some(LuaCommandReturn {
                    output,
                    submit: true,
                    action: None,
                })
            }
        }
        mlua::Value::Table(t) => {
            let output = t
                .get::<Option<String>>("display")
                .ok()
                .flatten()
                .or_else(|| t.get::<Option<String>>("reply").ok().flatten())
                .or_else(|| t.get::<Option<String>>("content").ok().flatten())
                .unwrap_or_default();
            let action = parse_lua_return_action(&t);
            if output.is_empty() && action.is_none() {
                None
            } else {
                let submit = t
                    .get::<Option<bool>>("submit")
                    .ok()
                    .flatten()
                    .unwrap_or(true);
                Some(LuaCommandReturn {
                    output,
                    submit,
                    action,
                })
            }
        }
        mlua::Value::Nil => None,
        other => Some(LuaCommandReturn {
            output: format!("{other:?}"),
            submit: true,
            action: None,
        }),
    }
}

fn lua_value_to_json(value: mlua::Value) -> mlua::Result<serde_json::Value> {
    Ok(match value {
        mlua::Value::Nil => serde_json::Value::Null,
        mlua::Value::Boolean(b) => serde_json::Value::Bool(b),
        mlua::Value::Integer(n) => serde_json::Value::Number(n.into()),
        mlua::Value::Number(n) => serde_json::Number::from_f64(n)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        mlua::Value::String(s) => serde_json::Value::String(s.to_str()?.to_string()),
        mlua::Value::Table(t) => {
            let mut array_items: Vec<(usize, serde_json::Value)> = Vec::new();
            let mut object = serde_json::Map::new();
            let mut array_only = true;

            for pair in t.pairs::<mlua::Value, mlua::Value>() {
                let (key, value) = pair?;
                let value = lua_value_to_json(value)?;
                match key {
                    mlua::Value::Integer(i) if i >= 1 => array_items.push((i as usize, value)),
                    mlua::Value::String(s) => {
                        array_only = false;
                        object.insert(s.to_str()?.to_string(), value);
                    }
                    other => {
                        array_only = false;
                        object.insert(format!("{other:?}"), value);
                    }
                }
            }

            if array_only {
                array_items.sort_by_key(|(i, _)| *i);
                if array_items
                    .iter()
                    .enumerate()
                    .all(|(idx, (i, _))| *i == idx + 1)
                {
                    serde_json::Value::Array(array_items.into_iter().map(|(_, v)| v).collect())
                } else {
                    for (i, v) in array_items {
                        object.insert(i.to_string(), v);
                    }
                    serde_json::Value::Object(object)
                }
            } else {
                for (i, v) in array_items {
                    object.insert(i.to_string(), v);
                }
                serde_json::Value::Object(object)
            }
        }
        _ => serde_json::Value::Null,
    })
}

/// Parse a `LuaReturnAction` from a Lua table returned by a handler.
/// Returns `None` when no recognized action key is present.
pub(crate) fn parse_lua_return_action(table: &mlua::Table) -> Option<LuaReturnAction> {
    let action_name: Option<String> = table.get::<Option<String>>("action").ok().flatten();

    match action_name.as_deref() {
        Some("conversation.replace") => {
            let messages = match table.get::<Option<mlua::Table>>("messages") {
                Ok(Some(t)) => parse_messages_table(&t),
                _ => {
                    eprintln!("bone-lua warn: conversation.replace missing messages; ignoring");
                    return None;
                }
            };
            if messages.is_empty() {
                eprintln!("bone-lua warn: conversation.replace has no valid messages; ignoring");
                return None;
            }
            Some(LuaReturnAction {
                conversation_replace: Some(messages),
                ..Default::default()
            })
        }
        Some("conversation.load") => {
            let messages = match table.get::<Option<mlua::Table>>("messages") {
                Ok(Some(t)) => parse_messages_table(&t),
                _ => {
                    eprintln!("bone-lua warn: conversation.load missing messages; ignoring");
                    return None;
                }
            };
            if messages.is_empty() {
                eprintln!("bone-lua warn: conversation.load has no valid messages; ignoring");
                return None;
            }
            let conversation_id: Option<i64> =
                table.get::<Option<i64>>("conversation_id").ok().flatten();
            Some(LuaReturnAction {
                conversation_load: Some(ConversationLoad {
                    messages,
                    conversation_id,
                }),
                ..Default::default()
            })
        }
        Some(other) => {
            eprintln!("bone-lua warn: unknown action '{other}'; ignoring");
            None
        }
        None => None,
    }
}

/// Parse a Lua array of message tables into `ChatMessage`s. Entries with an
/// unrecognized role are skipped. Shared by `conversation.replace` and
/// `conversation.load`.
fn parse_messages_table(messages_table: &mlua::Table) -> Vec<crate::llm::ChatMessage> {
    let mut messages = Vec::new();
    for entry in messages_table.sequence_values::<mlua::Table>() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let role_str: String = match entry.get::<Option<String>>("role") {
            Ok(Some(s)) => s,
            _ => continue,
        };
        let content: String = entry
            .get::<Option<String>>("content")
            .ok()
            .flatten()
            .unwrap_or_default();
        let role = match role_str.as_str() {
            "user" => crate::llm::ChatRole::User,
            "assistant" => crate::llm::ChatRole::Assistant,
            "tool" => crate::llm::ChatRole::Tool,
            _ => continue,
        };
        let name: Option<String> = entry.get::<Option<String>>("name").ok().flatten();
        let tool_call_id: Option<String> =
            entry.get::<Option<String>>("tool_call_id").ok().flatten();
        let mut msg = crate::llm::ChatMessage::new(role, content.clone());
        if let Some(calls_table) = entry
            .get::<Option<mlua::Table>>("tool_calls")
            .ok()
            .flatten()
        {
            for call in calls_table.sequence_values::<mlua::Table>() {
                let call = match call {
                    Ok(call) => call,
                    Err(_) => continue,
                };
                let id: String = call
                    .get::<Option<String>>("id")
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                let name: String = call
                    .get::<Option<String>>("name")
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                if id.is_empty() || name.is_empty() {
                    continue;
                }
                let arguments = call
                    .get::<mlua::Value>("arguments")
                    .ok()
                    .and_then(|v| lua_value_to_json(v).ok())
                    .unwrap_or(serde_json::Value::Null);
                msg.tool_calls.push(ToolCall {
                    id,
                    name,
                    arguments,
                });
            }
        }
        msg.name = name;
        msg.tool_call_id = tool_call_id;
        messages.push(msg);
    }
    messages
}
// ── Private helpers ─────────────────────────────────────────────────────────

/// Lock the Lua state and check that the `bone` global table exists.
fn guard_with_bone(lua_arc: &Arc<Mutex<Lua>>) -> Option<std::sync::MutexGuard<'_, Lua>> {
    let guard = lua_arc.lock().unwrap_or_else(|e| e.into_inner());
    if guard
        .globals()
        .get::<Option<mlua::Table>>("bone")
        .ok()
        .flatten()
        .is_none()
    {
        return None;
    }
    Some(guard)
}

/// Inner dispatch logic. When `blockable` is false, always returns
/// `EventDispatchResult::Continue`. Handler errors are logged but never
/// block (fail-open).
fn dispatch_event_inner(
    lua: &mlua::Lua,
    event_name: &str,
    payload: serde_json::Value,
    blockable: bool,
) -> EventDispatchResult {
    let bone = match lua.globals().get::<Option<mlua::Table>>("bone") {
        Ok(Some(t)) => t,
        _ => return EventDispatchResult::Continue,
    };

    let handlers = match bone.get::<Option<mlua::Table>>("_handlers") {
        Ok(Some(t)) => t,
        _ => return EventDispatchResult::Continue,
    };

    let event_handlers = match handlers.get::<Option<mlua::Table>>(event_name) {
        Ok(Some(t)) => t,
        _ => return EventDispatchResult::Continue,
    };

    // Build event payload table.
    let event_table = match lua.to_value(&payload) {
        Ok(mlua::Value::Table(t)) => t,
        Ok(_) => return EventDispatchResult::Continue,
        Err(_) => return EventDispatchResult::Continue,
    };

    // Build minimal ctx table with ui.notify.
    let ctx_table = match create_event_ctx(lua) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("bone-lua warn: event ctx creation failed: {e}");
            return EventDispatchResult::Continue;
        }
    };

    for handler in event_handlers.sequence_values::<mlua::Function>() {
        let handler = match handler {
            Ok(h) => h,
            Err(_) => continue,
        };

        match handler.call::<Option<mlua::Table>>((event_table.clone(), ctx_table.clone())) {
            Ok(Some(ret)) if blockable => {
                if let Ok(Some(true)) = ret.get::<Option<bool>>("block") {
                    let reason = ret
                        .get::<Option<String>>("reason")
                        .ok()
                        .flatten()
                        .unwrap_or_else(|| "blocked by Lua event handler".to_string());
                    return EventDispatchResult::Blocked { reason };
                }
            }
            Ok(_) => {}
            Err(e) => {
                // Fail-open: log and continue to next handler.
                eprintln!("bone-lua warn: event handler error for '{event_name}': {e}");
            }
        }
    }

    EventDispatchResult::Continue
}

/// Create a minimal `ctx` table for event handlers with `ui.notify`.
fn create_event_ctx(lua: &mlua::Lua) -> Result<mlua::Table, mlua::Error> {
    let ctx = lua.create_table()?;

    let ui = lua.create_table()?;
    let notify_fn = lua.create_function(|_, (msg, level): (String, Option<String>)| {
        let prefix = match level.as_deref() {
            Some("warn") | Some("warning") => "bone-lua warn",
            Some("error") => "bone-lua error",
            _ => "bone-lua",
        };
        eprintln!("{prefix}: {msg}");
        Ok(())
    })?;
    ui.set("notify", notify_fn)?;

    ctx.set("ui", ui)?;
    Ok(ctx)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg_table(lua: &Lua, role: &str, content: &str) -> mlua::Table {
        let t = lua.create_table().unwrap();
        t.set("role", role).unwrap();
        t.set("content", content).unwrap();
        t
    }

    #[test]
    fn parses_conversation_load_with_id() {
        let lua = Lua::new();
        let messages = lua.create_table().unwrap();
        messages.push(msg_table(&lua, "user", "hi")).unwrap();
        messages.push(msg_table(&lua, "assistant", "hello")).unwrap();
        let action = lua.create_table().unwrap();
        action.set("action", "conversation.load").unwrap();
        action.set("messages", messages).unwrap();
        action.set("conversation_id", 7i64).unwrap();

        let parsed = parse_lua_return_action(&action).expect("action parsed");
        assert!(parsed.conversation_replace.is_none());
        let load = parsed.conversation_load.expect("load payload");
        assert_eq!(load.conversation_id, Some(7));
        assert_eq!(load.messages.len(), 2);
        assert_eq!(load.messages[0].content, "hi");
    }

    #[test]
    fn conversation_replace_still_parses() {
        let lua = Lua::new();
        let messages = lua.create_table().unwrap();
        messages.push(msg_table(&lua, "user", "hi")).unwrap();
        let action = lua.create_table().unwrap();
        action.set("action", "conversation.replace").unwrap();
        action.set("messages", messages).unwrap();

        let parsed = parse_lua_return_action(&action).expect("action parsed");
        assert!(parsed.conversation_load.is_none());
        assert_eq!(parsed.conversation_replace.expect("replace").len(), 1);
    }

    #[test]
    fn conversation_load_without_id_is_none_id() {
        let lua = Lua::new();
        let messages = lua.create_table().unwrap();
        messages.push(msg_table(&lua, "user", "hi")).unwrap();
        let action = lua.create_table().unwrap();
        action.set("action", "conversation.load").unwrap();
        action.set("messages", messages).unwrap();

        let parsed = parse_lua_return_action(&action).expect("action parsed");
        let load = parsed.conversation_load.expect("load payload");
        assert_eq!(load.conversation_id, None);
    }
}

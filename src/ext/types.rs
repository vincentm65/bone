//! Extension system types.

use std::sync::{Arc, Mutex};

use mlua::{Lua, LuaSerdeExt};

use super::snapshots::{LuaConfigSnapshot, LuaKeymapSnapshot, LuaThemeSnapshot};

/// Result of dispatching an event through all Lua handlers.
#[derive(Debug, Clone)]
pub enum EventDispatchResult {
    /// No handler blocked; continue normally.
    Continue,
    /// A handler requested blocking.
    Blocked { reason: String },
}

/// Owning manager for the Lua VM and all registered extension data.
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
    ) -> Self {
        Self {
            lua,
            engine_ok,
            loaded,
            commands,
            config_snapshot,
            theme_snapshot,
            keymap_snapshot,
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

    /// Render the subagent live-pane by calling `bone._subagents_render(jobs)`.
    /// Returns `Some(serde_json::Value)` when the Lua hook exists and returns
    /// a table, `None` otherwise.
    pub fn render_subagent_pane(&self, jobs: &serde_json::Value) -> Option<serde_json::Value> {
        if !self.loaded {
            return None;
        }
        let lua = guard_with_bone(&self.lua)?;
        let bone = lua.globals().get::<mlua::Table>("bone").ok()?;
        let hook: mlua::Value = bone.get("_subagents_render").ok()?;
        let fn_ref = match hook {
            mlua::Value::Function(f) => f,
            _ => return None,
        };
        let jobs_lua = lua.to_value(jobs).ok()?;
        match fn_ref.call::<mlua::Value>(jobs_lua) {
            Ok(mlua::Value::Table(t)) => {
                // Convert Lua table back to serde_json::Value.
                lua.from_value(mlua::Value::Table(t)).ok()
            }
            _ => None,
        }
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

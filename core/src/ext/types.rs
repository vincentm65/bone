//! Extension system types.

use std::sync::{Arc, Mutex};

use mlua::{Lua, LuaSerdeExt};

use crate::config::settings::{BoneSettings, Settings, ThemeStyleSpec};
use crate::tools::ToolCall;
use bone_protocol::KeymapDispatchKind;

/// Options controlling the Lua boot context.
///
/// Exposed to Lua as `bone.agent_depth` and `bone.headless` before
/// `init.lua` and tool/command files execute, so scripts can adapt
/// (e.g. the subagent tool refuses to register inside sub-agent VMs).
#[derive(Clone, Default)]
pub struct BootOptions {
    /// Nesting depth of the agent owning this VM (0 = top-level).
    pub agent_depth: usize,
    /// True when running without the TUI (CLI/headless agent). Background
    /// job auto-injection is unavailable, so tools must block for results.
    pub headless: bool,
    /// Model name for the banner (e.g. "gpt-4o").
    pub model: String,
    /// Provider name for the banner (e.g. "OpenAI (openai)").
    pub provider: String,
    /// Per-agent tool allowlist. When set, the booted tool handler only
    /// exposes tools whose names appear here (intersected with the globally
    /// enabled set). `None` (the default) exposes all enabled tools.
    pub tool_allowlist: Option<Vec<String>>,
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
    /// Text appended to the system prompt for this turn (e.g. a "plan mode"
    /// instruction). Returned by `before_turn`; stacks after the base prompt.
    ///
    /// Use only for text that is stable for the life of a conversation: the
    /// system prompt renders *before* the whole message history, so any
    /// turn-to-turn variation here invalidates the provider's prefix cache for
    /// every request. Turn-varying state belongs in `turn_message`.
    pub system_prompt_append: Option<String>,
    /// Transient message appended as the *last* input item of this turn's
    /// provider requests (not persisted to the transcript). Returned by
    /// `before_turn`. Because it sits at the tail of the prompt, its content
    /// can change every turn without breaking the provider's prefix cache —
    /// use it for turn-varying nudges (task-list state, goal iteration, etc.).
    pub turn_message: Option<String>,
    /// When set, only these tool names are exposed to the model for this turn
    /// (a per-turn allow-list). Returned by `before_turn`; an empty list hides
    /// every tool. Filters what the model *sees*, not the approval policy.
    pub tool_filter: Option<Vec<String>>,
    /// Config/runtime mutation requested by an interactive Lua command. These
    /// are applied by the TUI `App` after the Lua command returns.
    pub config_action: Option<ConfigAction>,
}

/// Payload for the `conversation.load` action (`/history`).
#[derive(Debug, Clone)]
pub struct ConversationLoad {
    pub messages: Vec<crate::llm::ChatMessage>,
    /// Conversation id to resume; future messages append here.
    pub conversation_id: Option<i64>,
}

#[derive(Debug, Clone)]
pub enum ConfigAction {
    Apply,
    ApplyRestartRequired,
    ReloadTools,
    SwitchProvider { id: String },
}

impl LuaReturnAction {
    /// Project the command-relevant fields onto the wire type so the daemon can
    /// forward this action to a remote client (`RuntimeEvent::CommandComplete`).
    /// Returns `None` when no command-relevant field is set (the
    /// `before_turn`-only `system_prompt_append`/`turn_message`/`tool_filter`
    /// fields are dropped — they never reach the command path).
    pub fn to_command_action(&self) -> Option<bone_protocol::CommandAction> {
        if self.conversation_replace.is_none()
            && self.conversation_load.is_none()
            && self.config_action.is_none()
        {
            return None;
        }
        Some(bone_protocol::CommandAction {
            conversation_replace: self.conversation_replace.clone(),
            conversation_load: self.conversation_load.as_ref().map(|l| {
                bone_protocol::ConversationLoad {
                    messages: l.messages.clone(),
                    conversation_id: l.conversation_id,
                }
            }),
            config_action: self.config_action.as_ref().map(|c| match c {
                ConfigAction::Apply => bone_protocol::ConfigAction::Apply,
                ConfigAction::ApplyRestartRequired => {
                    bone_protocol::ConfigAction::ApplyRestartRequired
                }
                ConfigAction::ReloadTools => bone_protocol::ConfigAction::ReloadTools,
                ConfigAction::SwitchProvider { id } => {
                    bone_protocol::ConfigAction::SwitchProvider { id: id.clone() }
                }
            }),
        })
    }
}

impl From<bone_protocol::CommandAction> for LuaReturnAction {
    /// Rebuild an action received over the wire so the client can apply it via
    /// `App::apply_lua_action`, exactly as the local path does.
    fn from(a: bone_protocol::CommandAction) -> Self {
        LuaReturnAction {
            conversation_replace: a.conversation_replace,
            conversation_load: a.conversation_load.map(|l| ConversationLoad {
                messages: l.messages,
                conversation_id: l.conversation_id,
            }),
            config_action: a.config_action.map(|c| match c {
                bone_protocol::ConfigAction::Apply => ConfigAction::Apply,
                bone_protocol::ConfigAction::ApplyRestartRequired => {
                    ConfigAction::ApplyRestartRequired
                }
                bone_protocol::ConfigAction::ReloadTools => ConfigAction::ReloadTools,
                bone_protocol::ConfigAction::SwitchProvider { id } => {
                    ConfigAction::SwitchProvider { id }
                }
            }),
            ..Default::default()
        }
    }
}

/// Normalized result from a Lua command handler.
#[derive(Debug, Clone)]
pub struct LuaCommandReturn {
    pub output: String,
    pub submit: bool,
    pub action: Option<LuaReturnAction>,
    /// Display role for non-submitted command output. Defaults to system.
    /// `assistant` renders Markdown like a normal assistant response.
    pub display_role: Option<String>,
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
    /// Commands registered via `bone.command.register()` during init.lua.
    commands: Vec<super::ops_commands::RegisteredLuaCommand>,
    /// Canonical resolved settings owned by this daemon runtime.
    settings: Arc<Mutex<Settings>>,
    /// Standalone shared UI-state handle. Lives outside the Lua VM mutex so
    /// the TUI can drain diffs even while a tool blocks on `ctx.ui.key()`.
    /// Also cloned into every `ctx.ui.pane` closure.
    ui: super::api_ui::SharedUi,
}

impl ExtensionManager {
    /// Wrap a pre-created `Arc<Mutex<Lua>>`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_arc(
        lua: Arc<Mutex<Lua>>,
        engine_ok: bool,
        loaded: bool,
        commands: Vec<super::ops_commands::RegisteredLuaCommand>,
        settings: Arc<Mutex<Settings>>,
        ui: super::api_ui::SharedUi,
    ) -> Self {
        Self {
            lua,
            engine_ok,
            loaded,
            commands,
            settings,
            ui,
        }
    }

    /// Construct a no-op manager with no Lua extensions loaded.
    ///
    /// This is the injection seam that lets the agent loop be driven and
    /// unit-tested **without** a config directory, a booted Lua VM, or any
    /// `init.lua`. Because every dispatch method early-returns when
    /// `!self.loaded`, such a manager provably does nothing: no hooks fire,
    /// `dispatch_tool_call` returns `Continue`, `dispatch_before_turn` returns
    /// an empty action list.
    ///
    /// It is exactly the fallback `boot()` already built internally on engine
    /// failure (`loader.rs`): a fresh `mlua::Lua::new()`, `engine_ok = false`,
    /// `loaded = false`, empty commands, default snapshots. Exposing
    /// it publicly just makes that same construction reachable from tests and
    /// (eventually) a headless/Driver path that does not own a Lua runtime.
    pub fn unloaded() -> Self {
        Self {
            lua: Arc::new(Mutex::new(Lua::new())),
            engine_ok: false,
            loaded: false,
            commands: Vec::new(),
            settings: Arc::new(Mutex::new(Settings::defaults())),
            ui: super::api_ui::new_shared(),
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
    pub fn lua_handle(&self) -> Arc<Mutex<Lua>> {
        self.lua.clone()
    }

    /// Clone the underlying Lua state handle (public for testing).
    pub fn lua_arc(&self) -> Arc<Mutex<Lua>> {
        self.lua_handle()
    }
    /// Clone the standalone shared UI-state handle.
    pub fn ui_handle(&self) -> super::api_ui::SharedUi {
        self.ui.clone()
    }

    /// Take the pending UI diffs emitted by `bone.api.ui.*`, `ctx.ui.pane`, and
    /// `ctx.ui.pane` since the last drain. A frontend calls this each render
    /// tick and applies the diffs to its own view (the TUI converts `Float`
    /// components to panes). Empty when no Lua UI calls have happened.
    ///
    /// Locks the standalone `UiState` mutex only — never the Lua VM mutex — so
    /// this is safe to call even while a tool blocks on `ctx.ui.key()` holding
    /// the VM lock.
    pub fn drain_view_diffs(&self) -> Vec<crate::runtime::view::ViewDiff> {
        super::api_ui::drain_diffs(&self.ui)
    }

    /// Get registered Lua commands.
    pub fn commands(&self) -> &[super::ops_commands::RegisteredLuaCommand] {
        &self.commands
    }

    /// Get the daemon-owned resolved settings, including dynamic UI highlights
    /// and renderer presets from the booted UI module.
    pub fn frontend_settings(&self) -> super::snapshots::ResolvedFrontendSettings {
        let mut settings = self
            .settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .resolved()
            .clone();
        let view = super::api_ui::snapshot(&self.ui);
        for (name, color) in view.highlights {
            if name == "bg" {
                settings.theme.palette.bg = Some(color);
            } else {
                settings
                    .theme
                    .highlights
                    .insert(name, ThemeStyleSpec::Color(color));
            }
        }
        let (spinner_styles, spinner_texts) = self
            .lua
            .lock()
            .map(|lua| super::snapshots::collect_presets(&lua))
            .unwrap_or_default();
        super::snapshots::ResolvedFrontendSettings {
            settings,
            spinner_styles,
            spinner_texts,
        }
    }

    /// Replace the daemon's resolved settings after a validated full-file reload.
    pub fn replace_settings(&self, settings: BoneSettings) {
        self.settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .inner = settings;
    }

    /// The base banner lines from `bone.banner()` (no client-side update/catalog
    /// hints — those stay a frontend concern). Empty if `banner()` is undefined
    /// or errors. Lets the daemon ship the banner to a VM-less frontend.
    pub fn frontend_banner(&self) -> String {
        let lua = self.lua_handle();
        let Ok(g) = lua.lock() else {
            return String::new();
        };
        let Ok(bone) = g.globals().get::<mlua::Table>("bone") else {
            return String::new();
        };
        let Ok(banner_fn) = bone.get::<mlua::Function>("banner") else {
            return String::new();
        };
        let mut lines = Vec::new();
        if let Ok(tbl) = banner_fn.call::<mlua::Table>(()) {
            for item in tbl.sequence_values::<mlua::String>().flatten() {
                if let Ok(s) = item.to_str() {
                    lines.push(s.to_string());
                }
            }
        }
        lines.join("\n")
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
                crate::ext::ctx::runtime_warn(format!(
                    "bone-lua warn: before_turn ctx creation failed: {e}"
                ));
                return Vec::new();
            }
        };

        // Event payload: minimal (the handler has ctx for everything).
        let event_table = match lua.create_table() {
            Ok(t) => t,
            Err(e) => {
                crate::ext::ctx::runtime_warn(format!(
                    "bone-lua warn: before_turn event table failed: {e}"
                ));
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
                    crate::ext::ctx::runtime_warn(format!(
                        "bone-lua warn: before_turn handler error: {e}"
                    ));
                }
            }
        }

        actions
    }

    /// Dispatch a keymap rhs: classify the action string (built-in, slash command,
    /// or prompt) or, if the action is a `__cb_<id>` callback reference, execute
    /// the Lua callback and classify its return value.
    ///
    /// Returns the classified kind so the frontend can apply it locally.
    pub fn dispatch_keymap(&self, action: &str) -> KeymapDispatchKind {
        // Lua callback reference: look up and execute.
        if let Some(cb_id) = action.strip_prefix("__cb_") {
            let lua = match guard_with_bone(&self.lua) {
                Some(g) => g,
                None => {
                    crate::ext::ctx::runtime_warn(format!(
                        "bone-lua warn: keymap callback {cb_id}: Lua unavailable"
                    ));
                    return KeymapDispatchKind::Noop;
                }
            };
            let bone = match lua.globals().get::<Option<mlua::Table>>("bone") {
                Ok(Some(t)) => t,
                _ => return KeymapDispatchKind::Noop,
            };
            let cbs: mlua::Table = match bone.get::<Option<mlua::Table>>("_keymap_callbacks") {
                Ok(Some(t)) => t,
                _ => return KeymapDispatchKind::Noop,
            };
            let ret: mlua::Value = match cbs.get::<mlua::Value>(cb_id) {
                Ok(v) => v,
                Err(_) => {
                    crate::ext::ctx::runtime_warn_once(format!(
                        "bone-lua warn: keymap callback {cb_id} not found"
                    ));
                    return KeymapDispatchKind::Noop;
                }
            };
            let result: String = match ret {
                mlua::Value::Function(f) => match f.call::<mlua::Value>(()) {
                    Ok(mlua::Value::String(s)) => {
                        s.to_str().map(|s| s.to_string()).unwrap_or_default()
                    }
                    Ok(mlua::Value::Nil) => return KeymapDispatchKind::Noop,
                    Ok(_) => {
                        crate::ext::ctx::runtime_warn_once(format!(
                            "bone-lua warn: keymap callback {cb_id} must return a string or nil"
                        ));
                        return KeymapDispatchKind::Noop;
                    }
                    Err(e) => {
                        crate::ext::ctx::runtime_warn(format!(
                            "bone-lua warn: keymap callback {cb_id} error: {e}"
                        ));
                        return KeymapDispatchKind::Noop;
                    }
                },
                _ => {
                    crate::ext::ctx::runtime_warn_once(format!(
                        "bone-lua warn: keymap callback {cb_id} is not callable"
                    ));
                    return KeymapDispatchKind::Noop;
                }
            };
            if result.is_empty() {
                KeymapDispatchKind::Noop
            } else {
                Self::classify_keymap_action(&result)
            }
        } else {
            Self::classify_keymap_action(action)
        }
    }

    /// Classify a keymap rhs string: built-in action, slash command, or prompt.
    fn classify_keymap_action(action: &str) -> KeymapDispatchKind {
        if action.starts_with('/') {
            KeymapDispatchKind::Command {
                text: action.to_string(),
            }
        } else if is_builtin_action(action) {
            KeymapDispatchKind::Builtin {
                action: action.to_string(),
            }
        } else {
            KeymapDispatchKind::Prompt {
                text: action.to_string(),
            }
        }
    }
}

/// Known built-in keymap actions the frontend can execute locally.
fn is_builtin_action(action: &str) -> bool {
    matches!(
        action,
        "toggle_panes"
            | "cycle_approval_mode"
            | "cursor_to_start"
            | "cursor_to_end"
            | "paste_image"
    )
}
pub struct BootResult {
    /// The extension manager (keeps the Lua VM alive).
    pub manager: ExtensionManager,
    /// Tools registered via `bone.tool.register()` during init.lua.
    pub tools: Vec<super::lua_tool::LuaTool>,
    /// Conversation-scoped `ctx.state` map shared by the collected Lua tools.
    /// The boot path installs this Arc on the resulting [`crate::tools::registry::ToolHandler`].
    pub shared_state: super::ctx::SharedState,
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
                    display_role: None,
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
                    display_role: t.get::<Option<String>>("display_role").ok().flatten(),
                })
            }
        }
        mlua::Value::Nil => None,
        other => Some(LuaCommandReturn {
            output: format!("{other:?}"),
            submit: true,
            action: None,
            display_role: None,
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
    let mut out = LuaReturnAction::default();
    let mut any = false;

    // Conversation mutation, keyed by the `action` field.
    let action_name: Option<String> = table.get::<Option<String>>("action").ok().flatten();
    match action_name.as_deref() {
        Some("conversation.replace") => {
            let messages = match table.get::<Option<mlua::Table>>("messages") {
                Ok(Some(t)) => parse_messages_table(&t),
                _ => Vec::new(),
            };
            if messages.is_empty() {
                crate::ext::ctx::runtime_warn_once(
                    "bone-lua warn: conversation.replace has no valid messages; ignoring",
                );
            } else {
                out.conversation_replace = Some(messages);
                any = true;
            }
        }
        Some("conversation.load") => {
            let conversation_id: Option<i64> =
                table.get::<Option<i64>>("conversation_id").ok().flatten();
            if conversation_id.is_none() {
                crate::ext::ctx::runtime_warn_once(
                    "bone-lua warn: conversation.load missing conversation_id; ignoring",
                );
            } else {
                // Accept messages from older commands for wire compatibility, but
                // the daemon is the sole authoritative transcript loader.
                let messages = match table.get::<Option<mlua::Table>>("messages") {
                    Ok(Some(t)) => parse_messages_table(&t),
                    _ => Vec::new(),
                };
                out.conversation_load = Some(ConversationLoad {
                    messages,
                    conversation_id,
                });
                any = true;
            }
        }
        Some("config.apply") => {
            out.config_action = Some(ConfigAction::Apply);
            any = true;
        }
        Some("config.apply_restart_required") => {
            out.config_action = Some(ConfigAction::ApplyRestartRequired);
            any = true;
        }
        Some("config.reload_tools") => {
            out.config_action = Some(ConfigAction::ReloadTools);
            any = true;
        }
        Some("config.switch_provider") => {
            let id = table
                .get::<Option<String>>("provider")
                .ok()
                .flatten()
                .or_else(|| table.get::<Option<String>>("id").ok().flatten())
                .unwrap_or_default();
            if id.is_empty() {
                crate::ext::ctx::runtime_warn_once(
                    "bone-lua warn: config.switch_provider missing provider id; ignoring",
                );
            } else {
                out.config_action = Some(ConfigAction::SwitchProvider { id });
                any = true;
            }
        }
        Some(other) => crate::ext::ctx::runtime_warn_once(format!(
            "bone-lua warn: unknown action '{other}'; ignoring"
        )),
        None => {}
    }

    // Turn-shaping fields, independent of `action` (so a handler can both
    // compact the conversation and shape the turn).
    if let Some(s) = table
        .get::<Option<String>>("system_prompt_append")
        .ok()
        .flatten()
        .filter(|s| !s.is_empty())
    {
        out.system_prompt_append = Some(s);
        any = true;
    }
    if let Some(s) = table
        .get::<Option<String>>("turn_message")
        .ok()
        .flatten()
        .filter(|s| !s.is_empty())
    {
        out.turn_message = Some(s);
        any = true;
    }
    if let Some(t) = table
        .get::<Option<Vec<String>>>("tool_filter")
        .ok()
        .flatten()
    {
        out.tool_filter = Some(t);
        any = true;
    }

    if any { Some(out) } else { None }
}

/// Parse a Lua array of message tables into `ChatMessage`s. Entries with an
/// unrecognized role are skipped. Shared by `conversation.replace` and
/// `conversation.load`.
pub(crate) fn parse_messages_table(messages_table: &mlua::Table) -> Vec<crate::llm::ChatMessage> {
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
        if let Some(images_table) = entry.get::<Option<mlua::Table>>("images").ok().flatten() {
            for image in images_table.sequence_values::<mlua::Table>() {
                let image = match image {
                    Ok(image) => image,
                    Err(_) => continue,
                };
                let media_type: String = image
                    .get::<Option<String>>("media_type")
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                let data: String = image
                    .get::<Option<String>>("data")
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                if media_type.is_empty() || data.is_empty() {
                    continue;
                }
                msg.images.push(crate::llm::ImageData { media_type, data });
            }
        }
        msg.name = name;
        msg.tool_call_id = tool_call_id;
        msg.is_error = entry
            .get::<Option<bool>>("is_error")
            .ok()
            .flatten()
            .unwrap_or(false);
        messages.push(msg);
    }
    messages
}
// ── Private helpers ─────────────────────────────────────────────────────────

/// Lock the Lua state and check that the `bone` global table exists.
fn guard_with_bone(lua_arc: &Arc<Mutex<Lua>>) -> Option<std::sync::MutexGuard<'_, Lua>> {
    let guard = lua_arc.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .globals()
        .get::<Option<mlua::Table>>("bone")
        .ok()
        .flatten()?;
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
            crate::ext::ctx::runtime_warn(format!("bone-lua warn: event ctx creation failed: {e}"));
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
                crate::ext::ctx::runtime_warn(format!(
                    "bone-lua warn: event handler error for '{event_name}': {e}"
                ));
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
        crate::ext::ctx::runtime_warn(format!("{prefix}: {msg}"));
        Ok(())
    })?;
    ui.set("notify", notify_fn)?;

    ctx.set("ui", ui)?;
    Ok(ctx)
}

#[cfg(test)]
#[path = "types_tests.rs"]
mod types_tests;

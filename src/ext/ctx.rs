//! ctx table — creates the context table passed to Lua tool `execute(params, ctx)`.
//!
//! Provides `shell`, `read_file`, `write_file` that delegate to the native
//! implementations with full policy enforcement.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use mlua::{Lua, LuaSerdeExt, Table, Value};

use crate::pane_content::InteractionMode;
use crate::tools::shell::{ScriptRequest, run_script};
use crate::tools::types::ToolCall;
use crate::tools::write_atomic::write_atomic;

/// Counter for synthetic Lua tool call IDs.
static LUA_CALL_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Shared mutable state accessible via ctx.state.
pub(crate) type SharedState = Arc<Mutex<HashMap<String, String>>>;

#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct UsageProviderContext {
    pub provider: String,
    pub model: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cached_tokens: u64,
    pub cost: f64,
    pub request_count: u64,
}

#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct UsageContext {
    pub request_count: u64,
    pub sent: u64,
    pub received: u64,
    pub cached: u64,
    pub cost: f64,
    pub context_length: u64,
    pub tool_count: u64,
    pub tool_schema_chars: u64,
    pub tool_schema_tokens: u64,
    pub system_prompt_chars: u64,
    pub system_prompt_tokens: u64,
    pub by_provider: Vec<UsageProviderContext>,
}

/// Context for creating the ctx table. These values come from the Rust side.
pub(crate) struct CtxConfig {
    pub config_dir: String,
    pub cwd: String,
    pub shared_state: SharedState,
    pub pane_sender: Option<tokio::sync::mpsc::UnboundedSender<crate::tools::types::ToolLiveEvent>>,
    pub call_id: Option<String>,
    pub tool_handler: Option<crate::tools::registry::ToolHandler>,
    pub approval_mode: crate::tools::ApprovalMode,
    pub tool_call_depth: usize,
    pub session_id: Option<i64>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub agent_depth: usize,
    pub cancelled: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    pub usage: Option<UsageContext>,
    pub conversation_history: Option<Vec<crate::llm::ChatMessage>>,
}

impl CtxConfig {
    /// Create a CtxConfig with default/inert values for all fields except
    /// `config_dir` and `shared_state`.
    pub fn new(config_dir: String, shared_state: SharedState) -> Self {
        Self {
            config_dir,
            cwd: std::env::current_dir()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            shared_state,
            pane_sender: None,
            call_id: None,
            tool_handler: None,
            approval_mode: crate::tools::ApprovalMode::Safe,
            tool_call_depth: 0,
            session_id: None,
            provider: None,
            model: None,
            agent_depth: 0,
            cancelled: None,
            usage: None,
            conversation_history: None,
        }
    }
}

/// A snapshot of the app-derived `ctx` fields shared by every Lua entry point
/// (slash commands, model-invoked tools, and the `before_turn` hook). Building
/// this in one place is the single source of truth for the fields that depend
/// on the running conversation, so commands and tools end up with an identical
/// `ctx`. Per-call fields (`pane_sender`, `call_id`, depths, `cancelled`) are
/// layered on by the caller, not stored here.
#[derive(Clone, Debug)]
pub(crate) struct AppCtxState {
    pub session_id: Option<i64>,
    pub provider: String,
    pub model: String,
    pub approval_mode: crate::tools::ApprovalMode,
    // Boxed to break the `ToolHandler` -> `AppCtxState` -> `ToolHandler` type
    // cycle (ToolHandler carries an `Option<AppCtxState>` snapshot).
    pub tool_handler: Box<crate::tools::registry::ToolHandler>,
    pub usage: UsageContext,
    pub conversation_history: Vec<crate::llm::ChatMessage>,
}

impl AppCtxState {
    /// Build a snapshot from the raw conversation pieces. Used both by the TUI
    /// (`App::app_ctx_state`) and the headless agent, so neither needs to know
    /// how the usage context is assembled.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tools: &crate::tools::registry::ToolHandler,
        stats: &crate::llm::TokenStats,
        approval_mode: crate::tools::ApprovalMode,
        session_id: Option<i64>,
        provider: &str,
        model: &str,
        by_provider: Vec<UsageProviderContext>,
        history: Vec<crate::llm::ChatMessage>,
    ) -> Self {
        let est = estimate_prompt_tokens(tools);
        Self {
            session_id,
            provider: provider.to_string(),
            model: model.to_string(),
            approval_mode,
            tool_handler: Box::new(tools.clone()),
            usage: build_usage_context(stats, &est, by_provider),
            conversation_history: history,
        }
    }

    /// Stamp the app-derived fields onto a `CtxConfig`. This is the one place
    /// that knows the field mapping; every entry point routes through it.
    pub fn apply_to(&self, cfg: &mut CtxConfig) {
        cfg.session_id = self.session_id;
        cfg.provider = Some(self.provider.clone());
        cfg.model = Some(self.model.clone());
        cfg.approval_mode = self.approval_mode;
        cfg.tool_handler = Some((*self.tool_handler).clone());
        cfg.usage = Some(self.usage.clone());
        cfg.conversation_history = Some(self.conversation_history.clone());
    }
}

/// Create the `ctx` table for a single tool invocation.
pub(crate) fn create_ctx_table(lua: &Lua, cfg: &CtxConfig) -> Result<Table, mlua::Error> {
    let ctx = lua.create_table()?;

    ctx.set("config_dir", cfg.config_dir.as_str())?;
    ctx.set("cwd", cfg.cwd.as_str())?;

    // ctx.log — print-to-stderr helpers
    let log_table = lua.create_table()?;
    for level in &["debug", "info", "warn", "error"] {
        let lvl = level.to_string();
        let log_fn = lua.create_function(move |lua, val: Value| {
            let msg: String = lua.from_value(val).unwrap_or_default();
            let _ = eprintln!("bone-lua [{lvl}]: {msg}");
            Ok(())
        })?;
        log_table.set(*level, log_fn)?;
    }
    ctx.set("log", log_table)?;

    // ctx.fs — filesystem helpers
    let fs_table = lua.create_table()?;

    // ctx.fs.exists(path) → bool
    let fs_exists = lua.create_function(|_, path: String| Ok(Path::new(&path).exists()))?;
    fs_table.set("exists", fs_exists)?;

    // ctx.fs.is_file(path) → bool
    let fs_is_file = lua.create_function(|_, path: String| Ok(Path::new(&path).is_file()))?;
    fs_table.set("is_file", fs_is_file)?;

    // ctx.fs.is_dir(path) → bool
    let fs_is_dir = lua.create_function(|_, path: String| Ok(Path::new(&path).is_dir()))?;
    fs_table.set("is_dir", fs_is_dir)?;

    // ctx.fs.read_dir(path) → array of {name, path, kind}
    let fs_read_dir = lua.create_function(|lua, path: String| {
        let entries = std::fs::read_dir(&path).map_err(|e| mlua::Error::external(e.to_string()))?;
        let mut vec: Vec<(String, String, String)> = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| mlua::Error::external(e.to_string()))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let entry_path = entry.path();
            let entry_path_str = entry_path.to_string_lossy().into_owned();
            let kind = if entry_path.is_file() {
                "file"
            } else if entry_path.is_dir() {
                "dir"
            } else {
                "other"
            };
            vec.push((name, entry_path_str, kind.to_string()));
        }
        vec.sort_by(|a, b| a.0.cmp(&b.0));
        let result = lua.create_table()?;
        for (name, entry_path, kind) in vec {
            let entry_table = lua.create_table()?;
            entry_table.set("name", name)?;
            entry_table.set("path", entry_path)?;
            entry_table.set("kind", kind)?;
            result.push(entry_table)?;
        }
        Ok(Value::Table(result))
    })?;
    fs_table.set("read_dir", fs_read_dir)?;

    // ctx.fs.metadata(path) → table or nil, error
    let fs_metadata = lua.create_function(|lua, path: String| {
        let meta = std::fs::metadata(&path).map_err(|e| mlua::Error::external(e.to_string()))?;
        let result = lua.create_table()?;
        result.set("path", Path::new(&path).to_string_lossy().into_owned())?;
        result.set(
            "kind",
            if meta.is_file() {
                "file"
            } else if meta.is_dir() {
                "dir"
            } else {
                "other"
            },
        )?;
        result.set("len", meta.len())?;
        result.set("readonly", meta.permissions().readonly())?;
        Ok(Value::Table(result))
    })?;
    fs_table.set("metadata", fs_metadata)?;

    ctx.set("fs", fs_table)?;

    // ctx.shell(command, opts?) → { stdout, stderr, exit_code }
    let shell_fn = lua.create_function(|lua, (command, opts): (String, Option<Table>)| {
        // Parse opts.
        let timeout_ms = opts
            .as_ref()
            .and_then(|t| t.get::<Option<u64>>("timeout_ms").ok().flatten())
            .unwrap_or(120_000)
            .clamp(1_000, 300_000);

        // We need to run an async function from this synchronous Lua callback.
        // Use block_in_place since we're inside a tokio runtime.
        let output = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                run_script(ScriptRequest {
                    command,
                    env: Vec::new(),
                    timeout_ms,
                })
                .await
            })
        });

        match output {
            Ok(out) => {
                let result = lua.create_table()?;
                result.set("stdout", out.stdout)?;
                result.set("stderr", out.stderr)?;
                result.set("exit_code", out.exit_code.map(|c| c as i64).unwrap_or(-1))?;
                Ok(Value::Table(result))
            }
            Err(e) => Err(mlua::Error::external(e)),
        }
    })?;
    ctx.set("shell", shell_fn)?;

    // ctx.shell_streaming(command, callback, opts?) → { stdout, stderr, exit_code }
    // Runs command via bash, reads stdout line-by-line, calls callback(line) for each.
    let shell_streaming_fn = lua.create_function(
        |lua, (command, callback, opts): (String, mlua::Function, Option<Table>)| {
            let timeout_ms = opts
                .as_ref()
                .and_then(|t| t.get::<Option<u64>>("timeout_ms").ok().flatten())
                .unwrap_or(300_000)
                .clamp(1_000, 300_000);

            use std::io::{BufRead, BufReader, Read};
            use std::process::{Command, Stdio};
            use std::sync::mpsc;
            use std::time::{Duration, Instant};

            let mut child = Command::new("bash")
                .arg("-c")
                .arg(&command)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| mlua::Error::external(format!("failed to spawn: {e}")))?;

            let stdout = child.stdout.take().unwrap();
            let stderr_handle = child.stderr.take().unwrap();

            // Spawn a reader thread that sends each stdout line through a channel.
            let (tx, rx) = mpsc::channel::<Result<String, String>>();
            let reader_thread = std::thread::spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines() {
                    match line {
                        Ok(l) => {
                            if tx.send(Ok(l)).is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(Err(e.to_string()));
                            break;
                        }
                    }
                }
            });

            // Also read stderr in the background so the child doesn't block on a full pipe.
            let stderr_thread = std::thread::spawn(move || {
                let mut buf = String::new();
                let _ = BufReader::new(stderr_handle).read_to_string(&mut buf);
                buf
            });

            let deadline = Instant::now() + Duration::from_millis(timeout_ms);
            let mut stdout_acc = String::new();
            let mut timed_out = false;

            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    timed_out = true;
                    break;
                }

                match rx.recv_timeout(remaining) {
                    Ok(Ok(line)) => {
                        callback.call::<()>(line.clone())?;
                        stdout_acc.push_str(&line);
                        stdout_acc.push('\n');
                    }
                    Ok(Err(e)) => {
                        return Err(mlua::Error::external(format!("read error: {e}")));
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        timed_out = true;
                        break;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        break;
                    }
                }
            }

            if timed_out {
                let _ = child.kill();
            }

            let _ = reader_thread.join();
            let stderr_content = stderr_thread.join().unwrap_or_default();

            let exit_status = child.wait().ok();

            let exit_code = if timed_out {
                -1i64
            } else {
                exit_status
                    .and_then(|s| s.code())
                    .map(|c| c as i64)
                    .unwrap_or(-1)
            };

            let result = lua.create_table()?;
            result.set("stdout", stdout_acc)?;
            result.set("stderr", stderr_content)?;
            result.set("exit_code", exit_code)?;
            Ok(Value::Table(result))
        },
    )?;
    ctx.set("shell_streaming", shell_streaming_fn)?;

    // ctx.read_file(path) → content string (raises a Lua error on failure)
    let read_fn = lua.create_function(|_, path: String| {
        // block_in_place for async fs::read_to_string
        let result = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(async { tokio::fs::read_to_string(&path).await })
        });
        match result {
            Ok(content) => Ok(content),
            Err(e) => Err(mlua::Error::external(e.to_string())),
        }
    })?;
    ctx.set("read_file", read_fn)?;

    // ctx.write_file(path, content) → true or nil, error_string
    let write_fn = lua.create_function(|_, (path, content): (String, String)| {
        let result = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                let path = Path::new(&path);
                // Reject if file exists — same policy as native write_file tool.
                if path.exists() {
                    return Err("file already exists; use edit_file for modifications".to_string());
                }
                // Create parent directories.
                if let Some(parent) = path.parent()
                    && !parent.as_os_str().is_empty()
                {
                    tokio::fs::create_dir_all(parent)
                        .await
                        .map_err(|e| e.to_string())?;
                }
                write_atomic(path, &content, None).await
            })
        });
        match result {
            Ok(()) => Ok(true),
            Err(e) => Err(mlua::Error::external(e)),
        }
    })?;
    ctx.set("write_file", write_fn)?;

    // ctx.ui.notify(message, level?) — write a notification-style stderr line
    // for warnings/errors. Info notifications are intentionally quiet so
    // background hooks do not corrupt the TUI.
    let ui_table = lua.create_table()?;
    let notify_fn = lua.create_function(|_, (msg, level): (String, Option<String>)| {
        match level.as_deref() {
            Some("warn") | Some("warning") => eprintln!("bone-lua warn: {msg}"),
            Some("error") => eprintln!("bone-lua error: {msg}"),
            _ => {}
        }
        Ok(())
    })?;
    ui_table.set("notify", notify_fn)?;

    // ctx.ui.status(message) — write a notification-style stderr line
    let status_fn = lua.create_function(|_, msg: String| {
        eprintln!("bone-lua: {msg}");
        Ok(())
    })?;
    ui_table.set("status", status_fn)?;

    // ctx.ui.pane(opts) — thin alias over emit_pane
    // Only works when pane_sender exists; returns false, "pane unavailable" otherwise.
    if let Some(sender) = cfg.pane_sender.clone() {
        let pane_fn = lua.create_function(move |lua, table: mlua::Table| {
            let val: serde_json::Value = lua.from_value(mlua::Value::Table(table))?;
            let pane = crate::pane_content::PaneContent::from_json(&val)
                .map_err(|e| mlua::Error::external(e))?;
            sender
                .send(crate::tools::types::ToolLiveEvent::Pane(pane))
                .map_err(|e| mlua::Error::external(format!("emit_pane send failed: {e}")))?;
            Ok(true)
        })?;
        ui_table.set("pane", pane_fn)?;
    } else {
        let pane_unavailable_fn =
            lua.create_function(|_, _: ()| Ok((false, "pane unavailable")))?;
        ui_table.set("pane", pane_unavailable_fn)?;
    }

    // ctx.ui.interact(opts) → table result
    // Creates an interactive pane and blocks until the user responds.
    // opts: { question, type, options?, default?, allow_custom? }
    if let Some(sender) = cfg.pane_sender.clone() {
        // Shared mutex serializes concurrent interact calls (e.g. multiple
        // ask_user tools dispatched in one batch) so they share the same page
        // instead of clobbering each other.
        static INTERACT_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let interact_fn = lua.create_function(move |lua, opts: mlua::Table| {
            let question: String = opts.get("question").map_err(|e| mlua::Error::external(format!("interact missing question: {e}")))?;
            let type_str: String = opts.get::<Option<String>>("type")?.unwrap_or_else(|| "single_select".to_string());
            let options: Vec<String> = opts.get::<Option<Vec<String>>>("options")?.unwrap_or_default();
            let default: Option<usize> = opts.get::<Option<usize>>("default")?;
            let allow_custom: bool = opts.get::<Option<bool>>("allow_custom")?.unwrap_or(false);

            let mode = match type_str.as_str() {
                "single_select" | "single" => InteractionMode::SingleSelect,
                "multi_select" | "multi" => InteractionMode::MultiSelect,
                "text_input" | "text" => InteractionMode::TextInput,
                other => return Err(mlua::Error::external(format!("interact: unknown type '{other}'. Valid: single_select, multi_select, text_input"))),
            };

            // Validate: MultiSelect and SingleSelect need options.
            if !matches!(mode, InteractionMode::TextInput) && options.is_empty() {
                return Err(mlua::Error::external("interact: options required for single_select and multi_select"));
            }

            // Custom input is always available for selection modes.
            let allow_custom = allow_custom || !matches!(mode, InteractionMode::TextInput);

            // Build the interaction request — the frontend constructs the
            // pane (ratatui Lines, PaneInteraction) from this pure-data type.
            let (tx, rx) = tokio::sync::oneshot::channel::<serde_json::Value>();
            let default_selected = default.map(|d| d.saturating_sub(1)).unwrap_or(0);
            let request = crate::pane_content::InteractRequest {
                question,
                mode,
                options,
                default_selected,
                allow_custom,
                reply: tx,
            };

            // Acquire the serialization lock, send the request, then block
            // for the user response — the lock is held for the entire
            // send+wait cycle so concurrent calls are serialized.
            let _lock = INTERACT_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
            sender
                .send(crate::tools::types::ToolLiveEvent::Interact(request))
                .map_err(|e| mlua::Error::external(format!("interact send failed: {e}")))?;

            // Block until the user responds (lock still held, serializing concurrent calls).
            let result: serde_json::Value = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(rx)
            }).map_err(|e| mlua::Error::external(format!("interact cancelled: {e}")))?;

            let lua_result = lua.to_value(&result)
                .map_err(|e| mlua::Error::external(format!("interact result conversion: {e}")))?;
            Ok(lua_result)
        })?;
        ui_table.set("interact", interact_fn)?;
    } else {
        let interact_unavailable_fn =
            lua.create_function(|_, _: ()| Ok((false, "interact unavailable")))?;
        ui_table.set("interact", interact_unavailable_fn)?;
    }

    ctx.set("ui", ui_table)?;

    // ctx.usage.snapshot() → current conversation usage details.
    let usage_table = lua.create_table()?;
    if let Some(usage) = cfg.usage.clone() {
        let snapshot_fn = lua.create_function(move |lua, _: ()| {
            let result = lua.create_table()?;
            result.set("request_count", usage.request_count)?;
            result.set("sent", usage.sent)?;
            result.set("received", usage.received)?;
            result.set("cached", usage.cached)?;
            result.set("cost", usage.cost)?;
            result.set("context_length", usage.context_length)?;
            result.set("tool_count", usage.tool_count)?;
            result.set("tool_schema_chars", usage.tool_schema_chars)?;
            result.set("tool_schema_tokens", usage.tool_schema_tokens)?;
            result.set("system_prompt_chars", usage.system_prompt_chars)?;
            result.set("system_prompt_tokens", usage.system_prompt_tokens)?;
            let by_provider = lua.create_table()?;
            for provider in &usage.by_provider {
                let row = lua.create_table()?;
                row.set("provider", provider.provider.clone())?;
                row.set("model", provider.model.clone())?;
                row.set("prompt_tokens", provider.prompt_tokens)?;
                row.set("completion_tokens", provider.completion_tokens)?;
                row.set("cached_tokens", provider.cached_tokens)?;
                row.set("cost", provider.cost)?;
                row.set("request_count", provider.request_count)?;
                by_provider.push(row)?;
            }
            result.set("by_provider", by_provider)?;
            Ok(Value::Table(result))
        })?;
        usage_table.set("snapshot", snapshot_fn)?;
    } else {
        let snapshot_fn = lua.create_function(|_, _: ()| Ok(Value::Nil))?;
        usage_table.set("snapshot", snapshot_fn)?;
    }
    ctx.set("usage", usage_table)?;

    // ctx.conversation — active conversation snapshot.
    // ctx.conversation.current() → table or nil
    // ctx.conversation.history() → array of {role, content, name?, tool_call_id?}
    let conversation_table = lua.create_table()?;
    if let Some(ref history) = cfg.conversation_history {
        let session_id = cfg.session_id;
        let provider = cfg.provider.clone();
        let model = cfg.model.clone();

        let current_fn = lua.create_function(move |lua, _: ()| match session_id {
            Some(id) => {
                let t = lua.create_table()?;
                t.set("id", id)?;
                if let Some(ref p) = provider {
                    t.set("provider", p.as_str())?;
                }
                if let Some(ref m) = model {
                    t.set("model", m.as_str())?;
                }
                Ok(Value::Table(t))
            }
            None => Ok(Value::Nil),
        })?;
        conversation_table.set("current", current_fn)?;

        let history_clone = history.clone();
        let history_fn = lua.create_function(move |lua, _: ()| {
            let tbl = lua.create_table()?;
            for msg in &history_clone {
                let entry = lua.create_table()?;
                entry.set("role", msg.role.as_str())?;
                entry.set("content", msg.content.as_str())?;
                if !msg.tool_calls.is_empty() {
                    let calls = lua.create_table()?;
                    for call in &msg.tool_calls {
                        let call_entry = lua.create_table()?;
                        call_entry.set("id", call.id.as_str())?;
                        call_entry.set("name", call.name.as_str())?;
                        call_entry.set("arguments", lua.to_value(&call.arguments)?)?;
                        calls.push(call_entry)?;
                    }
                    entry.set("tool_calls", calls)?;
                }
                if let Some(ref name) = msg.name {
                    entry.set("name", name.as_str())?;
                }
                if let Some(ref tci) = msg.tool_call_id {
                    entry.set("tool_call_id", tci.as_str())?;
                }
                tbl.push(entry)?;
            }
            Ok(Value::Table(tbl))
        })?;
        conversation_table.set("history", history_fn)?;
    } else {
        let nil_fn = lua.create_function(|_, _: ()| Ok(Value::Nil))?;
        conversation_table.set("current", nil_fn.clone())?;
        conversation_table.set("history", nil_fn)?;
    }
    ctx.set("conversation", conversation_table)?;

    // ctx.state.get(key) → string or nil
    // ctx.state.set(key, value) → true
    // ctx.state.clear(key) → true
    let state_get = lua.create_function({
        let state_ref = cfg.shared_state.clone();
        move |_, key: String| {
            let map = state_ref
                .lock()
                .map_err(|e| mlua::Error::external(e.to_string()))?;
            Ok(map.get(&key).cloned())
        }
    })?;
    let state_set = lua.create_function({
        let state_ref = cfg.shared_state.clone();
        move |_, (key, value): (String, String)| {
            let mut map = state_ref
                .lock()
                .map_err(|e| mlua::Error::external(e.to_string()))?;
            map.insert(key, value);
            Ok(true)
        }
    })?;
    let state_clear = lua.create_function({
        let state_ref = cfg.shared_state.clone();
        move |_, key: String| {
            let mut map = state_ref
                .lock()
                .map_err(|e| mlua::Error::external(e.to_string()))?;
            map.remove(&key);
            Ok(true)
        }
    })?;
    let state_table = lua.create_table()?;
    state_table.set("get", state_get)?;
    state_table.set("set", state_set)?;
    state_table.set("clear", state_clear)?;
    ctx.set("state", state_table)?;

    // ctx.tools — invoke registered tool definitions and call tools directly
    let tools_table = lua.create_table()?;

    // ctx.tools.definitions() → array of {name, description, input_schema}
    let defs = if let Some(ref handler) = cfg.tool_handler {
        handler.definitions()
    } else {
        Vec::new()
    };
    let defs_fn = lua.create_function(move |lua, _: ()| {
        let tbl = lua.create_table()?;
        for def in &defs {
            let entry = lua.create_table()?;
            entry.set("name", def.name.clone())?;
            entry.set("description", def.description.clone())?;
            let schema: mlua::Value = lua.to_value(&def.input_schema)?;
            entry.set("input_schema", schema)?;
            tbl.push(entry)?;
        }
        Ok(Value::Table(tbl))
    })?;
    tools_table.set("definitions", defs_fn)?;

    // ctx.tools.call(name, args, opts?) → { ok, name, call_id, content, is_error }
    if let Some(ref handler) = cfg.tool_handler {
        let mut handler = handler.clone();
        let pane_sender = cfg.pane_sender.clone();
        let depth = cfg.tool_call_depth;
        let agent_depth = cfg.agent_depth;
        // Wire parent cancellation flag so tools stop if user cancels.
        handler.cancel_token = cfg.cancelled.clone();

        let call_fn = lua.create_function(
            move |lua, (name, args, opts): (String, mlua::Table, Option<mlua::Table>)| {
                // depth is the tool_call_depth of the *current* tool (captured
                // when this ctx table was created). If it is already at the
                // limit, reject the call. Otherwise pass depth + 1 to the
                // nested execution so that the next level gets its own
                // incremented depth in its ctx table.
                if depth >= MAX_TOOL_CALL_DEPTH {
                    let result = lua.create_table()?;
                    result.set("ok", false)?;
                    result.set("name", name)?;
                    result.set("call_id", Value::Nil)?;
                    result.set("content", "max tool call depth exceeded")?;
                    result.set("is_error", true)?;
                    return Ok(Value::Table(result));
                }

                // Determine approval mode: opts.approval or inherited
                let mode_str: Option<String> = opts
                    .as_ref()
                    .and_then(|t| t.get::<Option<String>>("approval").ok().flatten());
                // Generate synthetic call id (needed for error responses)
                let call_id = format!(
                    "lua-call-{}",
                    LUA_CALL_COUNTER.fetch_add(1, Ordering::Relaxed)
                );

                let mode = match mode_str.as_deref() {
                    Some("safe") | Some("read_only") => crate::tools::ApprovalMode::Safe,
                    Some("danger") => crate::tools::ApprovalMode::Danger,
                    _ => {
                        let mode_str = mode_str.as_deref().unwrap_or("(none)");
                        let result = lua.create_table()?;
                        result.set("ok", false)?;
                        result.set("name", name)?;
                        result.set("call_id", call_id.clone())?;
                        result.set("content", format!("Unknown approval mode: {mode_str}"))?;
                        result.set("is_error", true)?;
                        return Ok(Value::Table(result));
                    }
                };

                // Convert args table to serde_json::Value
                let args_val: serde_json::Value = lua.from_value(mlua::Value::Table(args))?;

                let call = ToolCall {
                    id: call_id.clone(),
                    name: name.clone(),
                    arguments: args_val,
                };

                if !handler.allows_call(mode, &call) {
                    let result = lua.create_table()?;
                    result.set("ok", false)?;
                    result.set("name", name)?;
                    result.set("call_id", call_id)?;
                    result.set(
                        "content",
                        "Tool not executed. Approval mode does not allow this call.",
                    )?;
                    result.set("is_error", true)?;
                    return Ok(Value::Table(result));
                }

                // Execute the tool synchronously (block_in_place).
                let results = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        handler
                            .execute_all_live(
                                vec![call],
                                pane_sender.clone(),
                                agent_depth,
                                depth + 1,
                            )
                            .await
                    })
                });

                if let Some(result) = results.into_iter().next() {
                    let out = lua.create_table()?;
                    out.set("ok", !result.is_error)?;
                    out.set("name", result.name)?;
                    out.set("call_id", result.call_id)?;
                    out.set("content", result.content)?;
                    out.set("is_error", result.is_error)?;
                    Ok(Value::Table(out))
                } else {
                    let out = lua.create_table()?;
                    out.set("ok", false)?;
                    out.set("name", name)?;
                    out.set("call_id", call_id)?;
                    out.set("content", "tool execution returned no results")?;
                    out.set("is_error", true)?;
                    Ok(Value::Table(out))
                }
            },
        )?;
        tools_table.set("call", call_fn)?;
    } else {
        let no_handler_fn = lua.create_function(|lua, _: ()| {
            let out = lua.create_table()?;
            out.set("ok", false)?;
            out.set("name", Value::Nil)?;
            out.set("call_id", Value::Nil)?;
            out.set("content", "tools unavailable")?;
            out.set("is_error", true)?;
            Ok(Value::Table(out))
        })?;
        tools_table.set("call", no_handler_fn)?;
    }

    ctx.set("tools", tools_table)?;

    // ctx.agent.run(prompt, opts?) → { ok, content, error }
    add_agent_table(&lua, &ctx, cfg)?;

    // ctx.config — read-only access to configuration.
    let config_table = lua.create_table()?;
    config_table.set("dir", cfg.config_dir.as_str())?;

    // ctx.config.get(section, key)
    let config_dir_for_get = cfg.config_dir.clone();
    let config_get_fn = lua.create_function(move |lua, (section, key): (String, String)| {
        let path = std::path::Path::new(&config_dir_for_get)
            .join("config")
            .join(format!("{section}.yaml"));
        if !path.is_file() {
            return Ok(Value::Nil);
        }
        let content = std::fs::read_to_string(&path)
            .map_err(|e| mlua::Error::external(format!("failed to read config: {e}")))?;
        let doc: serde_yaml::Value = serde_yaml::from_str(&content)
            .map_err(|e| mlua::Error::external(format!("invalid YAML in {section}.yaml: {e}")))?;
        let Some(mapping) = doc.as_mapping() else {
            return Ok(Value::Nil);
        };
        // Look in fields array for matching key, then top-level mapping.
        if let Some(fields) = mapping.get(&serde_yaml::Value::String("fields".into()))
            && let Some(fields_arr) = fields.as_sequence()
        {
            for field in fields_arr {
                if let Some(field_map) = field.as_mapping() {
                    let field_key = field_map.get(&serde_yaml::Value::String("key".into()));
                    if field_key.and_then(|v| v.as_str()) == Some(&key) {
                        // Prefer "value", fall back to "default".
                        let val = field_map
                            .get(&serde_yaml::Value::String("value".into()))
                            .or_else(|| {
                                field_map.get(&serde_yaml::Value::String("default".into()))
                            });
                        if let Some(v) = val {
                            return yaml_to_lua(lua, v);
                        }
                    }
                }
            }
        }
        // Fall back to top-level key in the mapping.
        if let Some(v) = mapping.get(&serde_yaml::Value::String(key.clone())) {
            return yaml_to_lua(lua, v);
        }
        Ok(Value::Nil)
    })?;
    config_table.set("get", config_get_fn)?;

    // ctx.config.get_table(section)
    let config_dir_for_table = cfg.config_dir.clone();
    let config_get_table_fn = lua.create_function(move |lua, section: String| {
        let path = std::path::Path::new(&config_dir_for_table)
            .join("config")
            .join(format!("{section}.yaml"));
        if !path.is_file() {
            return Ok(Value::Nil);
        }
        let content = std::fs::read_to_string(&path)
            .map_err(|e| mlua::Error::external(format!("failed to read config: {e}")))?;
        let doc: serde_yaml::Value = serde_yaml::from_str(&content)
            .map_err(|e| mlua::Error::external(format!("invalid YAML in {section}.yaml: {e}")))?;
        yaml_to_lua(lua, &doc)
    })?;
    config_table.set("get_table", config_get_table_fn)?;

    ctx.set("config", config_table)?;

    // ctx.session — read-only access to session/conversation history.
    let session_table = lua.create_table()?;

    // ctx.session.current() → table or nil
    let current_session_id = cfg.session_id;
    let current_provider = cfg.provider.clone();
    let current_model = cfg.model.clone();
    let session_current_fn = lua.create_function(move |lua, _: ()| match current_session_id {
        Some(id) => {
            let t = lua.create_table()?;
            t.set("id", id)?;
            if let Some(ref p) = current_provider {
                t.set("provider", p.as_str())?;
            }
            if let Some(ref m) = current_model {
                t.set("model", m.as_str())?;
            }
            Ok(Value::Table(t))
        }
        None => Ok(Value::Nil),
    })?;
    session_table.set("current", session_current_fn)?;

    // ctx.session.list(opts?) → array of conversation summaries
    let session_list_fn = lua.create_function(move |lua, opts: Option<mlua::Table>| {
        let limit = opts
            .as_ref()
            .and_then(|t| t.get::<Option<usize>>("limit").ok().flatten())
            .unwrap_or(20)
            .clamp(1, 100);
        let db_path = crate::session_db::db_path();
        let db = crate::session_db::SessionDb::open(&db_path)
            .map_err(|e| mlua::Error::external(format!("failed to open session db: {e}")))?;
        let conversations = db
            .list_conversations(limit)
            .map_err(|e| mlua::Error::external(format!("failed to list conversations: {e}")))?;
        let result = lua.create_table()?;
        for conv in conversations {
            let t = lua.create_table()?;
            t.set("id", conv.id)?;
            t.set("provider", conv.provider)?;
            t.set("model", conv.model)?;
            t.set("started_at", conv.started_at)?;
            if let Some(ended) = conv.ended_at {
                t.set("ended_at", ended)?;
            }
            result.push(t)?;
        }
        Ok(Value::Table(result))
    })?;
    session_table.set("list", session_list_fn)?;

    // ctx.session.messages(conversation_id, opts?) → array of messages
    let session_messages_fn = lua.create_function(
        move |lua, (conversation_id, opts): (i64, Option<mlua::Table>)| {
            let limit = opts
                .as_ref()
                .and_then(|t| t.get::<Option<usize>>("limit").ok().flatten())
                .unwrap_or(200)
                .clamp(1, 1000);
            let db_path = crate::session_db::db_path();
            let db = crate::session_db::SessionDb::open(&db_path)
                .map_err(|e| mlua::Error::external(format!("failed to open session db: {e}")))?;
            let messages = db
                .list_messages(conversation_id, limit)
                .map_err(|e| mlua::Error::external(format!("failed to list messages: {e}")))?;
            let result = lua.create_table()?;
            for msg in messages {
                let t = lua.create_table()?;
                t.set("seq", msg.seq)?;
                t.set("role", msg.role)?;
                t.set("content", msg.content)?;
                if let Some(tn) = msg.tool_name {
                    t.set("tool_name", tn)?;
                }
                if let Some(tci) = msg.tool_call_id {
                    t.set("tool_call_id", tci)?;
                }
                if let Some(ref tc_json) = msg.tool_calls {
                    if let Ok(tc_vec) = serde_json::from_str::<Vec<serde_json::Value>>(tc_json) {
                        let tc_table = lua.create_table()?;
                        for tc_val in tc_vec {
                            let tc = lua.create_table()?;
                            if let Some(id) = tc_val.get("id") {
                                tc.set("id", lua.to_value(id)?)?;
                            }
                            if let Some(name) = tc_val.get("name") {
                                tc.set("name", lua.to_value(name)?)?;
                            }
                            if let Some(args) = tc_val.get("arguments") {
                                tc.set("arguments", lua.to_value(args)?)?;
                            }
                            tc_table.push(tc)?;
                        }
                        t.set("tool_calls", tc_table)?;
                    }
                }
                result.push(t)?;
            }
            Ok(Value::Table(result))
        },
    )?;
    session_table.set("messages", session_messages_fn)?;

    ctx.set("session", session_table)?;

    // ctx.db.query(sql, params?) — raw SQL query, returns array of row tables.
    let db_query_fn = lua.create_function(|lua, (sql, params): (String, Option<Vec<Value>>)| {
        let db_path = crate::session_db::db_path();
        let db = crate::session_db::SessionDb::open(&db_path)
            .map_err(|e| mlua::Error::external(format!("failed to open session db: {e}")))?;

        // Read-only: only allow SELECT statements.
        let sql_trimmed = sql.trim();
        if !sql_trimmed.to_lowercase().starts_with("select") {
            return Err(mlua::Error::external(
                "ctx.db.query only allows SELECT statements",
            ));
        }

        // Build bound parameters.
        let params: Vec<rusqlite::types::Value> = match &params {
            Some(p) => p
                .iter()
                .map(|v| match v {
                    Value::Integer(i) => rusqlite::types::Value::Integer(*i),
                    Value::Number(n) => rusqlite::types::Value::Real(*n),
                    Value::String(s) => rusqlite::types::Value::Text(
                        s.to_str()
                            .ok()
                            .and_then(|b| Some(b.to_string()))
                            .unwrap_or_default(),
                    ),
                    Value::Nil => rusqlite::types::Value::Null,
                    Value::Boolean(b) => rusqlite::types::Value::Integer(*b as i64),
                    _ => rusqlite::types::Value::Text(tostring_lua_value(v)),
                })
                .collect(),
            None => Vec::new(),
        };

        let mut stmt = db
            .conn_ref()
            .prepare(&sql)
            .map_err(|e| mlua::Error::external(format!("SQL error: {e}")))?;

        let columns: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();

        let mut rows = stmt
            .query(rusqlite::params_from_iter(&params))
            .map_err(|e| mlua::Error::external(format!("query error: {e}")))?;

        let result = lua.create_table()?;
        while let Some(row) = rows
            .next()
            .map_err(|e| mlua::Error::external(format!("row error: {e}")))?
        {
            let row_map = lua.create_table()?;
            for (i, col_name) in columns.iter().enumerate() {
                let val = row_to_lua_value(lua, row, i)?;
                row_map.set(col_name.as_str(), val)?;
            }
            result.push(row_map)?;
        }
        Ok(Value::Table(result))
    })?;
    let db_table = lua.create_table()?;
    db_table.set("query", db_query_fn)?;
    ctx.set("db", db_table)?;

    // ctx.call_id — the tool call's unique ID (available during execute_output_live).
    if let Some(cid) = &cfg.call_id {
        ctx.set("call_id", cid.as_str())?;
    }
    // ctx.emit_pane(table) — send a live pane update during tool execution.
    // Only works when called from execute_output_live (sender is Some).
    if let Some(sender) = cfg.pane_sender.clone() {
        let emit_pane_fn = lua.create_function(move |lua, table: mlua::Table| {
            let val: serde_json::Value = lua.from_value(mlua::Value::Table(table))?;
            let pane = crate::pane_content::PaneContent::from_json(&val)
                .map_err(|e| mlua::Error::external(e))?;
            sender
                .send(crate::tools::types::ToolLiveEvent::Pane(pane))
                .map_err(|e| mlua::Error::external(format!("emit_pane send failed: {e}")))?;
            Ok(true)
        })?;
        ctx.set("emit_pane", emit_pane_fn)?;
    }

    Ok(ctx)
}

/// Maximum nesting depth for subagent calls. Sub-agents cannot spawn
/// further sub-agents: only the top-level agent (depth 0) may delegate.
const MAX_AGENT_DEPTH: usize = 1;
/// Maximum nesting depth for tool calls from Lua.
const MAX_TOOL_CALL_DEPTH: usize = 4;
/// Default and max *inactivity* timeout for subagent calls. An agent only
/// times out when it has produced no observable progress (stream chunks,
/// tool results) for this long — not after a hard wall-clock cutoff.
const DEFAULT_AGENT_TIMEOUT_MS: u64 = 300_000;
const MAX_AGENT_TIMEOUT_MS: u64 = 900_000;

/// Resolves once the shared activity timestamp has been stale for
/// `timeout_ms` milliseconds. Used as an inactivity watchdog alongside
/// `run_agent` in a `select!`.
async fn inactivity_elapsed(activity: Arc<AtomicU64>, timeout_ms: u64) {
    loop {
        let last = activity.load(Ordering::Relaxed);
        let now = crate::agent::now_epoch_ms();
        let idle = now.saturating_sub(last);
        if idle >= timeout_ms {
            return;
        }
        let remaining = timeout_ms - idle;
        tokio::time::sleep(std::time::Duration::from_millis(remaining.min(1_000))).await;
    }
}

/// Create the `ctx.agent` table with `run` and `run_stream` functions.
fn add_agent_table(lua: &Lua, ctx: &Table, cfg: &CtxConfig) -> Result<(), mlua::Error> {
    let agent_table = lua.create_table()?;

    // Clone needed fields for the 'static closures.
    let inherited_approval = cfg.approval_mode;
    let inherited_provider = cfg.provider.clone();
    let inherited_model = cfg.model.clone();
    let agent_depth = cfg.agent_depth;
    let cancelled_flag = cfg.cancelled.clone();

    // Extra clones for spawn_fn (run_fn and run_stream_fn each move their own copies).
    let inherited_approval_j = inherited_approval;
    let inherited_provider_j = inherited_provider.clone();
    let inherited_model_j = inherited_model.clone();

    // --- ctx.agent.run(prompt, opts?) ---
    let run_fn = lua.create_function(move |lua, (prompt, opts): (String, Option<Table>)| {
        if agent_depth >= MAX_AGENT_DEPTH {
            return agent_depth_exceeded(lua);
        }

        let (approval, provider, model, system_prompt, timeout_ms) = match parse_agent_opts(
            &opts,
            inherited_approval,
            &inherited_provider,
            &inherited_model,
            RUN_OPT_KEYS,
        ) {
            Ok(v) => v,
            Err(e) => {
                let result = lua.create_table()?;
                result.set("ok", false)?;
                result.set("content", "")?;
                result.set("error", e)?;
                return Ok(Value::Table(result));
            }
        };

        let activity = Arc::new(AtomicU64::new(crate::agent::now_epoch_ms()));
        let request = crate::agent::AgentRequest {
            prompt,
            approval_mode: approval,
            provider,
            model,
            system_prompt,
            events: false,
            event_sender: None,
            agent_depth: agent_depth + 1,
            on_token_usage: None,
            activity: Some(activity.clone()),
            llm: None,
            session_sink: None,
        };

        let cancelled = cancelled_flag.clone();
        let response = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                tokio::select! {
                    result = crate::agent::run_agent(request) => result,
                    _ = await_cancelled(&cancelled) => Err("cancelled".to_string()),
                    _ = inactivity_elapsed(activity, timeout_ms) => {
                        Err(inactivity_message(timeout_ms))
                    }
                }
            })
        });

        agent_result_to_lua(lua, response)
    })?;
    agent_table.set("run", run_fn)?;

    // --- ctx.agent.run_stream(prompt, opts?) ---
    // Re-clone for the stream closure (same captures).
    let inherited_approval_s = cfg.approval_mode;
    let inherited_provider_s = cfg.provider.clone();
    let inherited_model_s = cfg.model.clone();
    let agent_depth_s = cfg.agent_depth;
    let cancelled_flag_s = cfg.cancelled.clone();
    let run_stream_fn =
        lua.create_function(move |lua, (prompt, opts): (String, Option<Table>)| {
            if agent_depth_s >= MAX_AGENT_DEPTH {
                return agent_depth_exceeded(lua);
            }

            let (approval, provider, model, system_prompt, timeout_ms) = match parse_agent_opts(
                &opts,
                inherited_approval_s,
                &inherited_provider_s,
                &inherited_model_s,
                RUN_STREAM_OPT_KEYS,
            ) {
                Ok(v) => v,
                Err(e) => {
                    let result = lua.create_table()?;
                    result.set("ok", false)?;
                    result.set("content", "")?;
                    result.set("error", e)?;
                    return Ok(Value::Table(result));
                }
            };

            // Extract Lua callbacks from opts.
            let on_started = opts_cb(&opts, "on_started");
            let on_status = opts_cb(&opts, "on_status");
            let on_tool_call = opts_cb(&opts, "on_tool_call");
            let on_tool_result = opts_cb(&opts, "on_tool_result");
            let on_token_usage = opts_cb(&opts, "on_token_usage");
            let on_finished = opts_cb(&opts, "on_finished");
            let on_failed = opts_cb(&opts, "on_failed");

            let (tx, mut rx) =
                tokio::sync::mpsc::unbounded_channel::<crate::agent::AgentRunEvent>();
            let activity = Arc::new(AtomicU64::new(crate::agent::now_epoch_ms()));
            let request = crate::agent::AgentRequest {
                prompt,
                approval_mode: approval,
                provider,
                model,
                system_prompt,
                events: false,
                event_sender: Some(tx),
                agent_depth: agent_depth_s + 1,
                on_token_usage: None,
                activity: Some(activity.clone()),
                llm: None,
                session_sink: None,
            };

            let cancelled = cancelled_flag_s.clone();
            let response = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let agent_future = crate::agent::run_agent(request);
                    tokio::pin!(agent_future);

                    // Process events and agent result concurrently.
                    loop {
                        tokio::select! {
                            result = &mut agent_future => {
                                // Drain remaining events.
                                let mut drain_err: Option<String> = None;
                                loop {
                                    match rx.try_recv() {
                                        Ok(event) => {
                                            if let Err(e) = dispatch_event(lua, &event,
                                                &on_started, &on_status, &on_tool_call,
                                                &on_tool_result, &on_token_usage,
                                                &on_finished, &on_failed) {
                                                drain_err = Some(format!("callback error: {e}"));
                                                break;
                                            }
                                        }
                                        Err(_) => break,
                                    }
                                }
                                if let Some(e) = drain_err {
                                    break Err(e);
                                }
                                break result;
                            }
                            Some(event) = rx.recv() => {
                                if let Err(e) = dispatch_event(lua, &event,
                                    &on_started, &on_status, &on_tool_call,
                                    &on_tool_result, &on_token_usage,
                                    &on_finished, &on_failed) {
                                    break Err(format!("callback error: {e}"));
                                }
                            }
                            _ = await_cancelled(&cancelled) => {
                                // Drain remaining events.
                                let mut drain_err: Option<String> = None;
                                loop {
                                    match rx.try_recv() {
                                        Ok(event) => {
                                            if let Err(e) = dispatch_event(lua, &event,
                                                &on_started, &on_status, &on_tool_call,
                                                &on_tool_result, &on_token_usage,
                                                &on_finished, &on_failed) {
                                                drain_err = Some(format!("callback error: {e}"));
                                                break;
                                            }
                                        }
                                        Err(_) => break,
                                    }
                                }
                                if let Some(e) = drain_err {
                                    break Err(e);
                                }
                                break Err("cancelled".to_string());
                            }
                            _ = inactivity_elapsed(activity.clone(), timeout_ms) => {
                                break Err(inactivity_message(timeout_ms));
                            }
                        }
                    }
                })
            });

            agent_result_to_lua(lua, response)
        })?;
    agent_table.set("run_stream", run_stream_fn)?;

    // --- ctx.agent.spawn(prompt, opts?) ---
    // Dispatch a non-blocking background agent run. Results are queryable
    // via ctx.agent.jobs() or delivered through the TUI peek/mark flow.
    let spawn_fn = lua.create_function(move |lua, (prompt, opts): (String, Option<Table>)| {
        // Sub-agents (depth > 0) cannot spawn background jobs — their results
        // would inject into the wrong conversation. They can still use blocking
        // ctx.agent.run.
        if agent_depth > 0 {
            let result = lua.create_table()?;
            result.set("ok", false)?;
            result.set("error", "sub-agents cannot spawn background jobs")?;
            return Ok(Value::Table(result));
        }

        let (approval, provider, model, system_prompt, timeout_ms) = match parse_agent_opts(
            &opts,
            inherited_approval_j,
            &inherited_provider_j,
            &inherited_model_j,
            SPAWN_OPT_KEYS,
        ) {
            Ok(v) => v,
            Err(e) => {
                let result = lua.create_table()?;
                result.set("ok", false)?;
                result.set("error", e)?;
                return Ok(Value::Table(result));
            }
        };

        // Read agent name from opts (registered sub-agent name, default "").
        let agent_name: String = opts
            .as_ref()
            .and_then(|t| t.get::<Option<String>>("agent").ok().flatten())
            .unwrap_or_default();

        let handle = tokio::runtime::Handle::try_current()
            .map_err(|e| mlua::Error::external(format!("spawn requires a tokio runtime: {e}")))?;

        // Atomic busy check: rejects when this agent already has a running job.
        let id = match crate::ext::jobs::registry().create(agent_name.clone(), prompt.clone()) {
            Ok(id) => id,
            Err(e) => {
                let result = lua.create_table()?;
                result.set("ok", false)?;
                result.set("error", e)?;
                return Ok(Value::Table(result));
            }
        };
        let id_for_task = id.clone();
        let id_for_spawn = id.clone();

        // Token tracking: shared counters updated by run_agent callback.
        let token_sent = Arc::new(AtomicU64::new(0));
        let token_received = Arc::new(AtomicU64::new(0));
        let token_sent_clone = token_sent.clone();
        let token_received_clone = token_received.clone();

        let activity = Arc::new(AtomicU64::new(crate::agent::now_epoch_ms()));
        let request = crate::agent::AgentRequest {
            prompt,
            approval_mode: approval,
            provider,
            model,
            system_prompt,
            events: false,
            event_sender: None,
            agent_depth: agent_depth + 1,
            on_token_usage: Some(Arc::new(move |sent: u64, received: u64| {
                token_sent_clone.store(sent, Ordering::Relaxed);
                token_received_clone.store(received, Ordering::Relaxed);
                crate::ext::jobs::registry().update_tokens(&id_for_task, sent, received);
            })),
            activity: Some(activity.clone()),
            llm: None,
            session_sink: None,
        };

        handle.spawn(async move {
            let outcome = tokio::select! {
                result = crate::agent::run_agent(request) => {
                    result.map(|resp| resp.content)
                }
                _ = inactivity_elapsed(activity, timeout_ms) => {
                    Err(format!("{id_for_spawn}: {}", inactivity_message(timeout_ms)))
                }
            };
            let ts = token_sent.load(Ordering::Relaxed);
            let tr = token_received.load(Ordering::Relaxed);
            crate::ext::jobs::registry().complete_with_tokens(&id_for_spawn, outcome, ts, tr);
        });

        let result = lua.create_table()?;
        result.set("ok", true)?;
        result.set("id", id.as_str())?;
        result.set("error", Value::Nil)?;
        Ok(Value::Table(result))
    })?;
    agent_table.set("spawn", spawn_fn)?;

    // --- ctx.agent.jobs() ---
    // Return a JSON array of all jobs (snapshot).
    let jobs_fn = lua.create_function(|lua, _: ()| {
        let snap = crate::ext::jobs::registry().snapshot();
        lua.to_value(&snap)
    })?;
    agent_table.set("jobs", jobs_fn)?;

    // --- ctx.agent.wait(ids?, opts?) ---
    // Block until the given background jobs finish (or all running jobs when
    // ids is nil/empty). Finished jobs are returned and marked consumed so
    // they are not auto-injected again. Esc (cancellation) aborts the wait;
    // the jobs themselves keep running and auto-inject later.
    let agent_depth_w = cfg.agent_depth;
    let cancelled_flag_w = cfg.cancelled.clone();
    let wait_fn = lua.create_function(
        move |lua, (ids, opts): (Option<Vec<String>>, Option<Table>)| {
            // Background jobs belong to the main conversation; sub-agents
            // can neither spawn nor wait on them.
            if agent_depth_w > 0 {
                let result = lua.create_table()?;
                result.set("ok", false)?;
                result.set("error", "sub-agents cannot wait on background jobs")?;
                return Ok(Value::Table(result));
            }

            let timeout_ms = opts
                .as_ref()
                .and_then(|t| t.get::<Option<u64>>("timeout_ms").ok().flatten())
                .unwrap_or(DEFAULT_AGENT_TIMEOUT_MS)
                .clamp(1_000, MAX_AGENT_TIMEOUT_MS);

            let registry = crate::ext::jobs::registry();
            let ids = match ids {
                Some(v) if !v.is_empty() => v,
                _ => registry.running_ids(),
            };

            let result = lua.create_table()?;
            result.set("ok", true)?;
            if ids.is_empty() {
                result.set("jobs", lua.create_table()?)?;
                result.set("pending", lua.create_table()?)?;
                result.set("timed_out", false)?;
                result.set("cancelled", false)?;
                return Ok(Value::Table(result));
            }

            // Blocking is safe here: top-level Lua tools run on a
            // spawn_blocking thread, and background jobs run on the tokio
            // runtime with their own Lua VMs (no lock shared with this one).
            let outcome = registry.wait_for(
                &ids,
                std::time::Duration::from_millis(timeout_ms),
                cancelled_flag_w.as_deref(),
            );

            let finished_json =
                serde_json::to_value(&outcome.finished).unwrap_or_else(|_| serde_json::json!([]));
            result.set("jobs", lua.to_value(&finished_json)?)?;
            result.set("pending", outcome.pending)?;
            result.set("timed_out", outcome.timed_out)?;
            result.set("cancelled", outcome.cancelled)?;
            Ok(Value::Table(result))
        },
    )?;
    agent_table.set("wait", wait_fn)?;

    ctx.set("agent", agent_table)?;
    Ok(())
}

/// Return `{ ok=false, content="", error="max agent depth exceeded" }`.
fn agent_depth_exceeded(lua: &Lua) -> Result<Value, mlua::Error> {
    let result = lua.create_table()?;
    result.set("ok", false)?;
    result.set("content", "")?;
    result.set("error", "max agent depth exceeded")?;
    Ok(Value::Table(result))
}

/// Known option keys per `ctx.agent` call, used to warn on typos.
const RUN_OPT_KEYS: &[&str] = &[
    "approval",
    "provider",
    "model",
    "system_prompt",
    "timeout_ms",
];
const RUN_STREAM_OPT_KEYS: &[&str] = &[
    "approval",
    "provider",
    "model",
    "system_prompt",
    "timeout_ms",
    "on_started",
    "on_status",
    "on_tool_call",
    "on_tool_result",
    "on_token_usage",
    "on_finished",
    "on_failed",
];
const SPAWN_OPT_KEYS: &[&str] = &[
    "approval",
    "provider",
    "model",
    "system_prompt",
    "timeout_ms",
    "agent",
];

/// Human-readable inactivity timeout message.
fn inactivity_message(timeout_ms: u64) -> String {
    format!(
        "agent timed out after {}s of inactivity (no stream or tool progress)",
        timeout_ms / 1000
    )
}

/// Warn (stderr) about unrecognized option keys so typos don't silently
/// fall back to defaults.
fn warn_unknown_opts(opts: &Option<Table>, allowed: &[&str]) {
    let Some(table) = opts else { return };
    for pair in table.clone().pairs::<Value, Value>() {
        let Ok((key, _)) = pair else { continue };
        if let Value::String(s) = key
            && let Ok(k) = s.to_str()
            && !allowed.contains(&k.as_ref())
        {
            eprintln!(
                "bone-lua warn: unknown agent option '{}' (known: {})",
                k.as_ref(),
                allowed.join(", ")
            );
        }
    }
}

/// Parse common opts for `run`, `run_stream`, and `spawn`.
fn parse_agent_opts(
    opts: &Option<Table>,
    inherited_approval: crate::tools::ApprovalMode,
    inherited_provider: &Option<String>,
    inherited_model: &Option<String>,
    allowed_keys: &[&str],
) -> Result<
    (
        crate::tools::ApprovalMode,
        Option<String>,
        Option<String>,
        Option<String>,
        u64,
    ),
    String,
> {
    warn_unknown_opts(opts, allowed_keys);

    let approval = match opts
        .as_ref()
        .and_then(|t| t.get::<Option<String>>("approval").ok().flatten())
        .as_deref()
    {
        Some("safe") | Some("read_only") => crate::tools::ApprovalMode::Safe,
        Some("danger") => crate::tools::ApprovalMode::Danger,
        Some(s) => return Err(format!("Unknown approval mode: {s}")),
        None => inherited_approval,
    };

    let explicit_provider = opts
        .as_ref()
        .and_then(|t| t.get::<Option<String>>("provider").ok().flatten())
        .filter(|s| !s.is_empty());

    let provider = explicit_provider
        .clone()
        .or_else(|| inherited_provider.clone());

    let explicit_model = opts
        .as_ref()
        .and_then(|t| t.get::<Option<String>>("model").ok().flatten())
        .filter(|s| !s.is_empty());

    let model = explicit_model.or_else(|| {
        if explicit_provider.is_none() {
            inherited_model.clone()
        } else {
            None
        }
    });

    let system_prompt: Option<String> = opts
        .as_ref()
        .and_then(|t| t.get::<Option<String>>("system_prompt").ok().flatten());

    let timeout_ms = opts
        .as_ref()
        .and_then(|t| t.get::<Option<u64>>("timeout_ms").ok().flatten())
        .unwrap_or(DEFAULT_AGENT_TIMEOUT_MS)
        .clamp(1_000, MAX_AGENT_TIMEOUT_MS);

    Ok((approval, provider, model, system_prompt, timeout_ms))
}

/// Extract an optional callback from opts table.
fn opts_cb(opts: &Option<Table>, key: &str) -> Option<mlua::Function> {
    opts.as_ref()
        .and_then(|t| t.get::<Option<mlua::Function>>(key).ok().flatten())
}

/// Dispatch a single RuntimeEvent to the appropriate Lua callback.
fn dispatch_event(
    lua: &Lua,
    event: &crate::runtime::RuntimeEvent,
    on_started: &Option<mlua::Function>,
    on_status: &Option<mlua::Function>,
    on_tool_call: &Option<mlua::Function>,
    on_tool_result: &Option<mlua::Function>,
    on_token_usage: &Option<mlua::Function>,
    on_finished: &Option<mlua::Function>,
    on_failed: &Option<mlua::Function>,
) -> Result<(), mlua::Error> {
    use crate::runtime::RuntimeEvent;
    match event {
        RuntimeEvent::Started {
            approval,
            task,
            model,
        } => {
            if let Some(cb) = on_started {
                let t = lua.create_table()?;
                t.set("approval", approval.as_str())?;
                t.set("task", task.as_str())?;
                t.set("model", model.as_str())?;
                cb.call::<()>(Value::Table(t))?;
            }
        }
        RuntimeEvent::Status { message } => {
            if let Some(cb) = on_status {
                cb.call::<()>(message.as_str())?;
            }
        }
        RuntimeEvent::ToolCall { name, summary, .. } => {
            if let Some(cb) = on_tool_call {
                let t = lua.create_table()?;
                t.set("name", name.as_str())?;
                t.set("summary", summary.as_str())?;
                cb.call::<()>(Value::Table(t))?;
            }
        }
        RuntimeEvent::ToolResult { name, is_error, .. } => {
            if let Some(cb) = on_tool_result {
                let t = lua.create_table()?;
                t.set("name", name.as_str())?;
                t.set("is_error", *is_error)?;
                cb.call::<()>(Value::Table(t))?;
            }
        }
        RuntimeEvent::TokenUsage { sent, received } => {
            if let Some(cb) = on_token_usage {
                let t = lua.create_table()?;
                t.set("sent", *sent as i64)?;
                t.set("received", *received as i64)?;
                cb.call::<()>(Value::Table(t))?;
            }
        }
        RuntimeEvent::Finished { content } => {
            if let Some(cb) = on_finished {
                cb.call::<()>(content.as_str())?;
            }
        }
        RuntimeEvent::Failed { message } => {
            if let Some(cb) = on_failed {
                cb.call::<()>(message.as_str())?;
            }
        }
        RuntimeEvent::TextDelta { .. }
        | RuntimeEvent::ReasoningDelta { .. }
        | RuntimeEvent::Pane { .. }
        | RuntimeEvent::Interact { .. } => {}
    }
    Ok(())
}

/// Convert the agent result into a Lua return table.
fn agent_result_to_lua(
    lua: &Lua,
    response: Result<crate::agent::AgentResponse, String>,
) -> Result<Value, mlua::Error> {
    let result = lua.create_table()?;
    match response {
        Ok(resp) => {
            result.set("ok", true)?;
            result.set("content", resp.content)?;
            result.set("error", Value::Nil)?;
        }
        Err(e) => {
            result.set("ok", false)?;
            result.set("content", "")?;
            result.set("error", e)?;
        }
    }
    Ok(Value::Table(result))
}

/// Future that resolves when the cancellation flag is set.
async fn await_cancelled(flag: &Option<std::sync::Arc<std::sync::atomic::AtomicBool>>) {
    if let Some(f) = flag {
        while !f.load(std::sync::atomic::Ordering::Relaxed) {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    } else {
        std::future::pending::<()>().await;
    }
}

/// Convert a serde_yaml::Value to a Lua value.
fn yaml_to_lua(lua: &Lua, val: &serde_yaml::Value) -> Result<Value, mlua::Error> {
    match val {
        serde_yaml::Value::Null => Ok(Value::Nil),
        serde_yaml::Value::Bool(b) => Ok(Value::Boolean(*b)),
        serde_yaml::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(Value::Integer(i))
            } else if let Some(f) = n.as_f64() {
                Ok(Value::Number(f))
            } else {
                Ok(Value::Nil)
            }
        }
        serde_yaml::Value::String(s) => {
            let s = lua.create_string(s)?;
            Ok(Value::String(s))
        }
        serde_yaml::Value::Sequence(arr) => {
            let t = lua.create_table()?;
            for item in arr {
                t.push(yaml_to_lua(lua, item)?)?;
            }
            Ok(Value::Table(t))
        }
        serde_yaml::Value::Mapping(map) => {
            let t = lua.create_table()?;
            for (k, v) in map {
                let key_str = match k {
                    serde_yaml::Value::String(s) => s.as_str(),
                    serde_yaml::Value::Number(_) => continue,
                    _ => continue,
                };
                t.set(key_str, yaml_to_lua(lua, v)?)?;
            }
            Ok(Value::Table(t))
        }
        serde_yaml::Value::Tagged(tagged) => yaml_to_lua(lua, &tagged.value),
    }
}

/// Token-usage figures derived from the current tool schema and system prompt.
/// Shared by every site that fills a `UsageContext`.
pub(crate) struct PromptTokenEstimate {
    pub tool_count: u64,
    pub schema_chars: u64,
    pub schema_tokens: u64,
    pub sys_chars: u64,
    pub sys_tokens: u64,
}

/// Estimate token counts for the serialized tool schema and the system prompt.
pub(crate) fn estimate_prompt_tokens(
    tools: &crate::tools::registry::ToolHandler,
) -> PromptTokenEstimate {
    let defs = tools.definitions();
    let schema_chars = serde_json::to_string(&defs).unwrap_or_default().len() as u64;
    let sys_chars = crate::llm::prompts::system_prompt().len() as u64;
    PromptTokenEstimate {
        tool_count: defs.len() as u64,
        schema_chars,
        schema_tokens: estimate_tokens(schema_chars),
        sys_chars,
        sys_tokens: estimate_tokens(sys_chars),
    }
}

/// Char-count → token-count using the shared `CHARS_PER_TOKEN` heuristic.
fn estimate_tokens(chars: u64) -> u64 {
    (chars as f64 / crate::llm::token_tracker::CHARS_PER_TOKEN).ceil() as u64
}

/// Per-provider usage breakdown for the current conversation, mapped into the
/// Lua-facing `UsageProviderContext`. Empty when there is no session DB,
/// conversation, or the query fails.
pub(crate) fn usage_by_provider_context(
    db: Option<&crate::session_db::SessionDb>,
    session_id: Option<i64>,
) -> Vec<UsageProviderContext> {
    db.and_then(|db| session_id.and_then(|id| db.usage_by_provider(id).ok()))
        .unwrap_or_default()
        .into_iter()
        .map(|p| UsageProviderContext {
            provider: p.provider,
            model: p.model,
            prompt_tokens: p.prompt_tokens.max(0) as u64,
            completion_tokens: p.completion_tokens.max(0) as u64,
            cached_tokens: p.cached_tokens.max(0) as u64,
            cost: p.cost,
            request_count: p.request_count.max(0) as u64,
        })
        .collect()
}

/// Assemble a `UsageContext` from cumulative token stats, the prompt estimate,
/// and the per-provider breakdown.
pub(crate) fn build_usage_context(
    stats: &crate::llm::TokenStats,
    est: &PromptTokenEstimate,
    by_provider: Vec<UsageProviderContext>,
) -> UsageContext {
    UsageContext {
        request_count: stats.request_count,
        sent: stats.sent,
        received: stats.received,
        cached: stats.cached,
        cost: stats.cost,
        context_length: stats.context_length,
        tool_count: est.tool_count,
        tool_schema_chars: est.schema_chars,
        tool_schema_tokens: est.schema_tokens,
        system_prompt_chars: est.sys_chars,
        system_prompt_tokens: est.sys_tokens,
        by_provider,
    }
}

/// Build the `CtxConfig` passed to the `before_turn` hook before each provider
/// request, from a shared app-state snapshot. The snapshot is the single source
/// of truth for app-derived fields ([`AppCtxState::apply_to`]).
pub(crate) fn build_before_turn_config(state: &AppCtxState) -> CtxConfig {
    let shared_state: SharedState = Arc::new(Mutex::new(HashMap::new()));
    let mut cfg = CtxConfig::new(
        crate::config::bone_dir().to_string_lossy().to_string(),
        shared_state,
    );
    state.apply_to(&mut cfg);
    cfg
}

/// Convert a Lua Value to a String for SQL parameter binding fallback.
fn tostring_lua_value(v: &Value) -> String {
    match v {
        Value::Integer(i) => i.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s
            .to_str()
            .ok()
            .and_then(|b| Some(b.to_string()))
            .unwrap_or_default(),
        Value::Boolean(b) => b.to_string(),
        Value::Nil => "null".to_string(),
        _ => "<unsupported>".to_string(),
    }
}

/// Convert a rusqlite row column to a Lua Value.
fn row_to_lua_value(lua: &Lua, row: &rusqlite::Row, idx: usize) -> mlua::Result<Value> {
    if let Ok(v) = row.get::<usize, i64>(idx) {
        return Ok(Value::Integer(v));
    }
    if let Ok(v) = row.get::<usize, f64>(idx) {
        return Ok(Value::Number(v));
    }
    if let Ok(v) = row.get::<usize, String>(idx) {
        return Ok(Value::String(lua.create_string(&v)?));
    }
    if let Ok(v) = row.get::<usize, Option<i64>>(idx) {
        return match v {
            Some(n) => Ok(Value::Integer(n)),
            None => Ok(Value::Nil),
        };
    }
    if let Ok(v) = row.get::<usize, Option<f64>>(idx) {
        return match v {
            Some(n) => Ok(Value::Number(n)),
            None => Ok(Value::Nil),
        };
    }
    if let Ok(v) = row.get::<usize, Option<String>>(idx) {
        return match v {
            Some(s) => Ok(Value::String(lua.create_string(&s)?)),
            None => Ok(Value::Nil),
        };
    }
    if let Ok(v) = row.get::<usize, Option<Vec<u8>>>(idx) {
        return lua.to_value(&v);
    }
    Ok(Value::Nil)
}

#[cfg(test)]
#[path = "ctx_tests.rs"]
mod ctx_tests;

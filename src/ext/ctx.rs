//! ctx table — creates the context table passed to Lua tool `execute(params, ctx)`.
//!
//! Provides `shell`, `read_file`, `write_file` that delegate to the native
//! implementations with full policy enforcement.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use mlua::{Lua, LuaSerdeExt, Table, Value};

use crate::tools::shell::{ScriptRequest, run_script};
use crate::tools::types::ToolCall;
use crate::tools::write_atomic::write_atomic;
use crate::ui::pane_page::PanePage;

/// Counter for synthetic Lua tool call IDs.
static LUA_CALL_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Shared mutable state accessible via ctx.state.
pub(crate) type SharedState = Arc<Mutex<HashMap<String, String>>>;

#[derive(Clone, Debug)]
pub(crate) struct UsageProviderContext {
    pub provider: String,
    pub model: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cached_tokens: u64,
    pub cost: f64,
    pub request_count: u64,
}

#[derive(Clone, Debug)]
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
        }
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

    // ctx.ui.notify(message, level?) — show a notification in the UI
    // level: "info" (default), "warn", "error"
    let ui_table = lua.create_table()?;
    let notify_fn = lua.create_function(|_, (msg, level): (String, Option<String>)| {
        let level = level.unwrap_or_else(|| "info".to_string());
        let prefixed = match level.as_str() {
            "warn" => format!("[warn] {msg}"),
            "error" => format!("[error] {msg}"),
            _ => format!("[info] {msg}"),
        };
        eprintln!("bone-lua: {prefixed}");
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
            let pane = PanePage::from_json(&val).map_err(|e| mlua::Error::external(e))?;
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
                result.push(t)?;
            }
            Ok(Value::Table(result))
        },
    )?;
    session_table.set("messages", session_messages_fn)?;

    ctx.set("session", session_table)?;

    // ctx.call_id — the tool call's unique ID (available during execute_output_live).
    if let Some(cid) = &cfg.call_id {
        ctx.set("call_id", cid.as_str())?;
    }
    // ctx.emit_pane(table) — send a live pane update during tool execution.
    // Only works when called from execute_output_live (sender is Some).
    if let Some(sender) = cfg.pane_sender.clone() {
        let emit_pane_fn = lua.create_function(move |lua, table: mlua::Table| {
            let val: serde_json::Value = lua.from_value(mlua::Value::Table(table))?;
            let pane = PanePage::from_json(&val).map_err(|e| mlua::Error::external(e))?;
            sender
                .send(crate::tools::types::ToolLiveEvent::Pane(pane))
                .map_err(|e| mlua::Error::external(format!("emit_pane send failed: {e}")))?;
            Ok(true)
        })?;
        ctx.set("emit_pane", emit_pane_fn)?;
    }

    // ctx.emit_state(source, sub_key, state_json) — send a StateUpdate live event.
    // ctx.emit_state_remove(source, sub_key) — send a StateRemove live event.
    // Only works when called from execute_output_live (sender is Some).
    if let Some(sender) = cfg.pane_sender.clone() {
        let sender_clone = sender.clone();
        let emit_state_fn = lua.create_function(
            move |_, (source, sub_key, state): (String, String, String)| {
                sender_clone
                    .send(crate::tools::types::ToolLiveEvent::StateUpdate {
                        source,
                        sub_key,
                        state,
                    })
                    .map_err(|e| mlua::Error::external(format!("emit_state send failed: {e}")))?;
                Ok(true)
            },
        )?;
        ctx.set("emit_state", emit_state_fn)?;

        let emit_state_remove_fn =
            lua.create_function(move |_, (source, sub_key): (String, String)| {
                sender
                    .send(crate::tools::types::ToolLiveEvent::StateRemove { source, sub_key })
                    .map_err(|e| {
                        mlua::Error::external(format!("emit_state_remove send failed: {e}"))
                    })?;
                Ok(true)
            })?;
        ctx.set("emit_state_remove", emit_state_remove_fn)?;
    }

    Ok(ctx)
}

/// Maximum nesting depth for subagent calls.
const MAX_AGENT_DEPTH: usize = 3;
/// Maximum nesting depth for tool calls from Lua.
const MAX_TOOL_CALL_DEPTH: usize = 4;
/// Default and max timeout for subagent calls.
const DEFAULT_AGENT_TIMEOUT_MS: u64 = 300_000;
const MAX_AGENT_TIMEOUT_MS: u64 = 900_000;

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
        };

        let cancelled = cancelled_flag.clone();
        let response = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), async {
                    tokio::select! {
                        result = crate::agent::run_agent(request) => result,
                        _ = await_cancelled(&cancelled) => Err("cancelled".to_string()),
                    }
                })
                .await
            })
        });

        agent_result_to_lua(lua, response, timeout_ms)
    })?;
    agent_table.set("run", run_fn)?;

    // --- ctx.agent.run_stream(prompt, opts?) ---
    // Re-clone for the stream closure (same captures).
    let inherited_approval_s = cfg.approval_mode;
    let inherited_provider_s = cfg.provider.clone();
    let inherited_model_s = cfg.model.clone();
    let agent_depth_s = cfg.agent_depth;
    let cancelled_flag_s = cfg.cancelled.clone();
    let run_stream_fn = lua.create_function(
        move |lua, (prompt, opts): (String, Option<Table>)| {
            if agent_depth_s >= MAX_AGENT_DEPTH {
                return agent_depth_exceeded(lua);
            }

            let (approval, provider, model, system_prompt, timeout_ms) = match parse_agent_opts(&opts, inherited_approval_s, &inherited_provider_s, &inherited_model_s) {
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

            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<crate::agent::AgentRunEvent>();
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
            };

            let cancelled = cancelled_flag_s.clone();
            let response = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    tokio::time::timeout(
                        std::time::Duration::from_millis(timeout_ms),
                        async {
                            let agent_future = crate::agent::run_agent(request);
                            tokio::pin!(agent_future);

                            // Process events and agent result concurrently.
                            let agent_result = loop {
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
                                }
                            };
                            agent_result
                        },
                    ).await
                })
            });

            agent_result_to_lua(lua, response, timeout_ms)
        },
    )?;
    agent_table.set("run_stream", run_stream_fn)?;

    // --- ctx.agent.spawn(prompt, opts?) ---
    // Dispatch a non-blocking background agent run. Results are queryable
    // via ctx.agent.jobs() or taken via take_finished_unconsumed() by the TUI.
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

        let id = crate::ext::jobs::registry().create(agent_name.clone(), prompt.clone());
        let id_for_task = id.clone();
        let id_for_spawn = id.clone();

        // Token tracking: shared counters updated by run_agent callback.
        let token_sent = Arc::new(AtomicU64::new(0));
        let token_received = Arc::new(AtomicU64::new(0));
        let token_sent_clone = token_sent.clone();
        let token_received_clone = token_received.clone();

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
        };

        let timeout_duration = std::time::Duration::from_millis(timeout_ms);
        handle.spawn(async move {
            let outcome = tokio::time::timeout(timeout_duration, async {
                crate::agent::run_agent(request).await
            })
            .await
            .map_err(|_| format!("job {id_for_spawn} timed out after {timeout_ms}ms"))
            .and_then(|r| r.map(|resp| resp.content).map_err(|e| e.to_string()));
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

/// Parse common opts for both `run` and `run_stream`.
fn parse_agent_opts(
    opts: &Option<Table>,
    inherited_approval: crate::tools::ApprovalMode,
    inherited_provider: &Option<String>,
    inherited_model: &Option<String>,
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

    let provider = opts
        .as_ref()
        .and_then(|t| t.get::<Option<String>>("provider").ok().flatten())
        .or_else(|| inherited_provider.clone());

    let model = opts
        .as_ref()
        .and_then(|t| t.get::<Option<String>>("model").ok().flatten())
        .or_else(|| inherited_model.clone());

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

/// Dispatch a single AgentRunEvent to the appropriate Lua callback.
fn dispatch_event(
    lua: &Lua,
    event: &crate::agent::AgentRunEvent,
    on_started: &Option<mlua::Function>,
    on_status: &Option<mlua::Function>,
    on_tool_call: &Option<mlua::Function>,
    on_tool_result: &Option<mlua::Function>,
    on_token_usage: &Option<mlua::Function>,
    on_finished: &Option<mlua::Function>,
    on_failed: &Option<mlua::Function>,
) -> Result<(), mlua::Error> {
    use crate::agent::AgentRunEvent;
    match event {
        AgentRunEvent::Started {
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
        AgentRunEvent::Status { message } => {
            if let Some(cb) = on_status {
                cb.call::<()>(message.as_str())?;
            }
        }
        AgentRunEvent::ToolCall { name, summary } => {
            if let Some(cb) = on_tool_call {
                let t = lua.create_table()?;
                t.set("name", name.as_str())?;
                t.set("summary", summary.as_str())?;
                cb.call::<()>(Value::Table(t))?;
            }
        }
        AgentRunEvent::ToolResult { name, is_error } => {
            if let Some(cb) = on_tool_result {
                let t = lua.create_table()?;
                t.set("name", name.as_str())?;
                t.set("is_error", *is_error)?;
                cb.call::<()>(Value::Table(t))?;
            }
        }
        AgentRunEvent::TokenUsage { sent, received } => {
            if let Some(cb) = on_token_usage {
                let t = lua.create_table()?;
                t.set("sent", *sent as i64)?;
                t.set("received", *received as i64)?;
                cb.call::<()>(Value::Table(t))?;
            }
        }
        AgentRunEvent::Finished { content } => {
            if let Some(cb) = on_finished {
                cb.call::<()>(content.as_str())?;
            }
        }
        AgentRunEvent::Failed { message } => {
            if let Some(cb) = on_failed {
                cb.call::<()>(message.as_str())?;
            }
        }
    }
    Ok(())
}

/// Convert the timeout-wrapped agent result into a Lua return table.
fn agent_result_to_lua(
    lua: &Lua,
    response: Result<Result<crate::agent::AgentResponse, String>, tokio::time::error::Elapsed>,
    timeout_ms: u64,
) -> Result<Value, mlua::Error> {
    let result = lua.create_table()?;
    match response {
        Ok(Ok(resp)) => {
            result.set("ok", true)?;
            result.set("content", resp.content)?;
            result.set("error", Value::Nil)?;
        }
        Ok(Err(e)) => {
            result.set("ok", false)?;
            result.set("content", "")?;
            result.set("error", e)?;
        }
        Err(_) => {
            result.set("ok", false)?;
            result.set("content", "")?;
            result.set("error", format!("agent timed out after {timeout_ms}ms"))?;
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

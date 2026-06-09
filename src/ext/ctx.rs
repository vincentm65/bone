//! ctx table — creates the context table passed to Lua tool `execute(params, ctx)`.
//!
//! Provides `shell`, `read_file`, `write_file` that delegate to the native
//! implementations with full policy enforcement.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use mlua::{Lua, Table, Value};

use crate::tools::shell::{ScriptRequest, run_script};
use crate::tools::write_atomic::write_atomic;

/// Shared mutable state accessible via ctx.state.
pub(crate) type SharedState = Arc<Mutex<HashMap<String, String>>>;

/// Context for creating the ctx table. These values come from the Rust side.
pub(crate) struct CtxConfig {
    pub cwd: String,
    pub config_dir: String,
    pub shared_state: SharedState,
}

/// Create the `ctx` table for a single tool invocation.
pub(crate) fn create_ctx_table(
    lua: &Lua,
    cfg: &CtxConfig,
) -> Result<Table, mlua::Error> {
    let ctx = lua.create_table()?;

    ctx.set("cwd", cfg.cwd.as_str())?;
    ctx.set("config_dir", cfg.config_dir.as_str())?;

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
                result.set(
                    "exit_code",
                    out.exit_code
                        .map(|c| c as i64)
                        .unwrap_or(-1),
                )?;
                Ok(Value::Table(result))
            }
            Err(e) => Err(mlua::Error::external(e)),
        }
    })?;
    ctx.set("shell", shell_fn)?;

    // ctx.read_file(path) → content string or nil, error_string
    let read_fn = lua.create_function(|_, path: String| {
        // block_in_place for async fs::read_to_string
        let result = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                tokio::fs::read_to_string(&path).await
            })
        });
        match result {
            Ok(content) => Ok(content),
            Err(e) => Err(mlua::Error::external(e.to_string())),
        }
    })?;
    ctx.set("read_file", read_fn)?;

    // ctx.write_file(path, content) → true or nil, error_string
    let write_fn =
        lua.create_function(|_, (path, content): (String, String)| {
            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let path = Path::new(&path);
                    // Reject if file exists — same policy as native write_file tool.
                    if path.exists() {
                        return Err(
                            "file already exists; use edit_file for modifications"
                                .to_string(),
                        );
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
    ctx.set("ui", ui_table)?;

    // ctx.state.get(key) → string or nil
    // ctx.state.set(key, value) → true
    // ctx.state.clear(key) → true
    let state_get = lua.create_function({
        let state_ref = cfg.shared_state.clone();
        move |_, key: String| {
            let map = state_ref.lock().map_err(|e| mlua::Error::external(e.to_string()))?;
            Ok(map.get(&key).cloned())
        }
    })?;
    let state_set = lua.create_function({
        let state_ref = cfg.shared_state.clone();
        move |_, (key, value): (String, String)| {
            let mut map = state_ref.lock().map_err(|e| mlua::Error::external(e.to_string()))?;
            map.insert(key, value);
            Ok(true)
        }
    })?;
    let state_clear = lua.create_function({
        let state_ref = cfg.shared_state.clone();
        move |_, key: String| {
            let mut map = state_ref.lock().map_err(|e| mlua::Error::external(e.to_string()))?;
            map.remove(&key);
            Ok(true)
        }
    })?;
    let state_table = lua.create_table()?;
    state_table.set("get", state_get)?;
    state_table.set("set", state_set)?;
    state_table.set("clear", state_clear)?;
    ctx.set("state", state_table)?;

    Ok(ctx)
}

//! ctx table — creates the context table passed to Lua tool `execute(params, ctx)`.
//!
//! Provides `shell`, `read_file`, `write_file` that delegate to the native
//! implementations with full policy enforcement.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use mlua::{Lua, LuaSerdeExt, Table, Value};

use crate::tools::shell::{ScriptRequest, run_script};
use crate::tools::write_atomic::write_atomic;

const RUNTIME_OP_KEY: &str = "__bone_runtime_op";

/// Shared mutable state accessible via ctx.state.
pub(crate) type SharedState = Arc<Mutex<HashMap<String, String>>>;

pub(crate) fn runtime_op_key() -> &'static str {
    RUNTIME_OP_KEY
}

/// Context for creating the ctx table. These values come from the Rust side.
pub(crate) struct CtxConfig {
    pub cwd: String,
    pub config_dir: String,
    pub shared_state: SharedState,
    pub pane_sender: Option<tokio::sync::mpsc::UnboundedSender<crate::tools::types::ToolLiveEvent>>,
    pub call_id: Option<String>,
}

/// Create the `ctx` table for a single tool invocation.
pub(crate) fn create_ctx_table(lua: &Lua, cfg: &CtxConfig) -> Result<Table, mlua::Error> {
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

    // ctx.read_file(path) → content string or nil, error_string
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
    ctx.set("ui", ui_table)?;

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

    // ctx.call_id — the tool call's unique ID (available during execute_output_live).
    if let Some(cid) = &cfg.call_id {
        ctx.set("call_id", cid.as_str())?;
    }
    // ctx.emit_pane(table) — send a live pane update during tool execution.
    // Only works when called from execute_output_live (sender is Some).
    if let Some(sender) = cfg.pane_sender.clone() {
        let emit_pane_fn = lua.create_function(move |lua, table: mlua::Table| {
            let val: serde_json::Value = lua.from_value(mlua::Value::Table(table))?;
            let pane = pane_from_json(&val).map_err(|e| mlua::Error::external(e))?;
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

/// Parse a pane definition from a serde_json::Value (same format as the JSON envelope).
fn pane_from_json(val: &serde_json::Value) -> Result<crate::ui::pane_page::PanePage, String> {
    use ratatui::style::{Modifier, Style};
    use ratatui::text::{Line, Span};

    let pane = val.as_object().ok_or("pane must be an object")?;
    let source = pane
        .get("source")
        .and_then(|v| v.as_str())
        .ok_or("pane missing source")?
        .to_string();
    let title = pane
        .get("title")
        .and_then(|v| v.as_str())
        .ok_or("pane missing title")?
        .to_string();
    let visible_rows = pane
        .get("visible_rows")
        .and_then(|v| v.as_u64())
        .unwrap_or(8) as usize;
    let scroll = pane.get("scroll").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

    let lines: Vec<Line<'static>> = pane
        .get("lines")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|line_val| {
                    if let Some(text) = line_val.as_str() {
                        Some(Line::from(text.to_string()))
                    } else if let Some(obj) = line_val.as_object() {
                        let spans: Vec<Span<'static>> = obj
                            .get("spans")
                            .and_then(|v| v.as_array())
                            .map(|spans_arr| {
                                spans_arr
                                    .iter()
                                    .filter_map(|span_val| {
                                        let span_obj = span_val.as_object()?;
                                        let text = span_obj
                                            .get("text")
                                            .and_then(|v| v.as_str())?
                                            .to_string();
                                        let mut style = Style::default();
                                        if let Some(fg) =
                                            span_obj.get("fg").and_then(|v| v.as_str())
                                        {
                                            if let Some(c) = parse_color(fg) {
                                                style = style.fg(c);
                                            }
                                        }
                                        if let Some(mods) =
                                            span_obj.get("modifiers").and_then(|v| v.as_array())
                                        {
                                            for m in mods {
                                                if let Some(s) = m.as_str() {
                                                    match s {
                                                        "bold" => {
                                                            style =
                                                                style.add_modifier(Modifier::BOLD)
                                                        }
                                                        "dim" => {
                                                            style =
                                                                style.add_modifier(Modifier::DIM)
                                                        }
                                                        "italic" => {
                                                            style =
                                                                style.add_modifier(Modifier::ITALIC)
                                                        }
                                                        "strike" | "crossed_out" => {
                                                            style = style
                                                                .add_modifier(Modifier::CROSSED_OUT)
                                                        }
                                                        _ => {}
                                                    }
                                                }
                                            }
                                        }
                                        Some(Span::styled(text, style))
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        if spans.is_empty() {
                            Some(Line::from(""))
                        } else {
                            Some(Line::from(spans))
                        }
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(crate::ui::pane_page::PanePage {
        source,
        title,
        content: lines,
        visible_rows,
        scroll,
    })
}

fn parse_color(s: &str) -> Option<ratatui::style::Color> {
    match s {
        "black" => Some(ratatui::style::Color::Black),
        "red" => Some(ratatui::style::Color::Red),
        "green" => Some(ratatui::style::Color::Green),
        "yellow" => Some(ratatui::style::Color::Yellow),
        "blue" => Some(ratatui::style::Color::Blue),
        "magenta" => Some(ratatui::style::Color::Magenta),
        "cyan" => Some(ratatui::style::Color::Cyan),
        "gray" | "grey" => Some(ratatui::style::Color::Gray),
        "dark_gray" | "dark_grey" => Some(ratatui::style::Color::DarkGray),
        "white" => Some(ratatui::style::Color::White),
        "lightred" => Some(ratatui::style::Color::LightRed),
        "lightgreen" => Some(ratatui::style::Color::LightGreen),
        "lightyellow" => Some(ratatui::style::Color::LightYellow),
        "lightblue" => Some(ratatui::style::Color::LightBlue),
        "lightmagenta" => Some(ratatui::style::Color::LightMagenta),
        "lightcyan" => Some(ratatui::style::Color::LightCyan),
        hex if hex.starts_with('#') && hex.len() == 7 => {
            let r = u8::from_str_radix(&hex[1..3], 16).ok()?;
            let g = u8::from_str_radix(&hex[3..5], 16).ok()?;
            let b = u8::from_str_radix(&hex[5..7], 16).ok()?;
            Some(ratatui::style::Color::Rgb(r, g, b))
        }
        _ => None,
    }
}

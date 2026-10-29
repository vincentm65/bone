//! `bone.job` for frontend Lua: starting, writing to and stopping jobs
//! (`crate::jobs`), their callbacks and events as they run, and a `pty`
//! job's screen.

use bone_lua::to_lua;
use mlua::{Function, Lua, MultiValue, Table, Value};
use serde_json::{Value as Json, json};

use super::processes::{screen_size, screen_table};
use super::{Host, Owned, args, drop_callback, err, fire, opt, own, ret, store_callback};
use crate::jobs::{Callbacks, Command, Ctl, Event, Exit, Spec, State, Stdin, Stream, split_lines};
use crate::model::Item;

/// The job ops; `Err` gives the arguments back for an op that is not one.
pub fn dispatch<H: Host>(
    h: &mut H,
    lua: &Lua,
    op: &str,
    a: MultiValue,
) -> Result<mlua::Result<MultiValue>, MultiValue> {
    Ok(match op {
        "job_start" => start_op(h, lua, a),
        "job_cancel" => args::<u64>(lua, a).and_then(|id| ret(lua, cancel(h, id))),
        "job_write" => args::<(u64, mlua::String)>(lua, a)
            .and_then(|(id, data)| ret(lua, write(h, id, data.as_bytes().to_vec()))),
        "job_close_stdin" => args::<u64>(lua, a).and_then(|id| ret(lua, close_stdin(h, id))),
        "job_status" => args::<u64>(lua, a).and_then(|id| match h.jobs().get(id) {
            Some(j) => ret(lua, to_lua(lua, &j.status())?),
            None => ret(lua, Value::Nil),
        }),
        "job_list" => {
            let list: Vec<Json> = h.jobs().list.iter().map(|j| j.status()).collect();
            to_lua(lua, &Json::Array(list)).and_then(|v| ret(lua, v))
        }
        "job_screen" => screen_op(h, lua, a),
        _ => return Err(a),
    })
}

fn start_op<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let (cmd, opts): (Value, Option<Table>) = args(lua, a)?;
    let spec = job_spec(cmd, &opts)?;
    let mut stored = Vec::new();
    let mut callback = |h: &mut H, k: &str| -> mlua::Result<Option<u64>> {
        match opt(&opts, k)? {
            None => Ok(None),
            Some(Value::Function(f)) => {
                let id = store_callback(h, lua, f)?;
                stored.push(id);
                Ok(Some(id))
            }
            Some(_) => Err(err(format!("job {k} must be a function"))),
        }
    };
    let cbs = (|| {
        Ok::<_, mlua::Error>((
            callback(h, "on_stdout")?,
            callback(h, "on_stderr")?,
            callback(h, "on_exit")?,
        ))
    })();
    let (on_stdout, on_stderr, on_exit) = match cbs {
        Ok(c) => c,
        Err(e) => {
            for id in stored {
                drop_callback(lua, id)?;
            }
            return Err(e);
        }
    };
    let flag =
        |k: &str| -> mlua::Result<Option<bool>> { opts.as_ref().map_or(Ok(None), |o| o.get(k)) };
    let cb = Callbacks {
        name: opt(&opts, "name")?,
        lines: flag("lines")?.unwrap_or(false),
        // Without output callbacks, keep the output for on_exit.
        buffer: flag("buffer")?.unwrap_or(on_stdout.is_none() && on_stderr.is_none()),
        on_stdout,
        on_stderr,
        on_exit,
    };
    let id = start(h, spec, cb);
    own(h, Owned::Job(id));
    ret(lua, id)
}

fn screen_op<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let (id, opts): (u64, Option<Table>) = args(lua, a)?;
    // Without a size it stays the size it is.
    let size = (h.jobs().get(id)).and_then(|j| Some(j.term.as_ref()?.state()?.size()));
    let (rows, cols, scroll) = screen_size(&opts, size.unwrap_or((24, 80)))?;
    let Some((lines, scroll, max, state)) = screen(h, id, rows, cols, scroll) else {
        return ret(lua, Value::Nil);
    };
    let r = screen_table(lua, &lines, scroll, max)?;
    if let Value::Table(state) = to_lua(lua, &state)? {
        for kv in state.pairs::<Value, Value>() {
            let (k, v) = kv?;
            r.set(k, v)?;
        }
    }
    ret(lua, r)
}

/// Start a job; its callbacks run on the UI thread as it goes.
pub fn start<H: Host>(h: &mut H, spec: Spec, cb: Callbacks) -> u64 {
    let o = h.owners();
    *o.next_callback += 1;
    let id = *o.next_callback;
    let sink = h.job_sink();
    h.jobs().start(id, spec, cb, sink);
    h.changed();
    id
}

/// Ask a running job to stop, and drop its screen. Returns whether it was
/// running.
pub fn cancel<H: Host>(h: &mut H, id: u64) -> bool {
    let Some(j) = h.jobs().get_mut(id) else {
        return false;
    };
    j.term = None;
    j.running() && j.ctl.send(Ctl::Cancel).is_ok()
}

/// Write to a job's stdin (opened with `stdin = true`) or terminal.
pub fn write<H: Host>(h: &mut H, id: u64, data: Vec<u8>) -> bool {
    match h.jobs().get(id) {
        Some(j) if j.running() => j.ctl.send(Ctl::Write(data)).is_ok(),
        _ => false,
    }
}

pub fn close_stdin<H: Host>(h: &mut H, id: u64) -> bool {
    match h.jobs().get(id) {
        Some(j) if j.running() => j.ctl.send(Ctl::CloseStdin).is_ok(),
        _ => false,
    }
}

/// A `pty` job's screen at `rows` × `cols` (resizing its terminal to that
/// first), scrolled back `scroll` rows: its lines, the scroll, how far it
/// could scroll, and the state keys and paste depend on.
pub fn screen<H: Host>(
    h: &mut H,
    id: u64,
    rows: u16,
    cols: u16,
    scroll: usize,
) -> Option<(Vec<Vec<Item>>, usize, usize, Json)> {
    // The job and the theme are both the frontend's: take the term out to
    // draw it.
    let j = h.jobs().get_mut(id)?;
    let running = j.running();
    let mut term = j.term.take()?;
    if running && term.state().map(|s| s.size()) != Some((rows, cols)) {
        let _ = j.ctl.send(Ctl::Resize(rows, cols));
    }
    let (lines, scroll, max) = term.lines(rows, cols, scroll, h.looks().theme);
    let state = term.state().map(|s| {
        let (row, col) = s.cursor_position();
        let mouse = match s.mouse_protocol_mode() {
            vt100::MouseProtocolMode::None => "none",
            vt100::MouseProtocolMode::Press => "press",
            vt100::MouseProtocolMode::PressRelease => "press_release",
            vt100::MouseProtocolMode::ButtonMotion => "button_motion",
            vt100::MouseProtocolMode::AnyMotion => "any_motion",
        };
        let mouse_encoding = match s.mouse_protocol_encoding() {
            vt100::MouseProtocolEncoding::Default => "default",
            vt100::MouseProtocolEncoding::Utf8 => "utf8",
            vt100::MouseProtocolEncoding::Sgr => "sgr",
        };
        json!({
            "cursor": (!s.hide_cursor() && scroll == 0).then_some([row + 1, col + 1]),
            "title": s.title(),
            "app_cursor": s.application_cursor(),
            "bracketed_paste": s.bracketed_paste(),
            "alt_screen": s.alternate_screen(),
            "mouse": mouse,
            "mouse_encoding": mouse_encoding,
            "running": running,
        })
    });
    if let Some(j) = h.jobs().get_mut(id) {
        j.term = Some(term);
    }
    Some((lines, scroll, max, state?))
}

/// What a job's task reported, on the UI thread.
pub fn event<H: Host>(h: &mut H, id: u64, ev: Event) {
    match ev {
        Event::Started(pid) => started(h, id, pid),
        Event::Output(stream, chunk) => output(h, id, stream, &chunk),
        Event::Exited(exit) => exited(h, id, exit),
    }
}

fn started<H: Host>(h: &mut H, id: u64, pid: Option<u32>) {
    let Some(j) = h.jobs().get_mut(id) else {
        return;
    };
    j.pid = pid;
    let ev = json!({ "id": id, "name": j.name, "cmd": j.command, "pid": pid });
    fire(h, "job/started", ev);
}

fn output<H: Host>(h: &mut H, id: u64, stream: Stream, chunk: &[u8]) {
    let Some(j) = h.jobs().get_mut(id) else {
        return;
    };
    if let Some(term) = &mut j.term {
        let reply = term.feed(chunk);
        if !reply.is_empty() {
            let _ = j.ctl.send(Ctl::Write(reply));
        }
        let watched = j.on_stdout.is_some();
        if !watched {
            j.out.bytes += chunk.len() as u64;
        }
        h.changed();
        if !watched {
            return;
        }
    }
    let Some(j) = h.jobs().get_mut(id) else {
        return;
    };
    let (lines, buffer) = (j.lines, j.buffer);
    let (out, cb) = match stream {
        Stream::Stdout => (&mut j.out, j.on_stdout),
        Stream::Stderr => (&mut j.err, j.on_stderr),
    };
    let text = out.decode(chunk);
    if buffer {
        out.keep(&text);
    }
    let pieces = if lines {
        out.line.push_str(&text);
        split_lines(&mut out.line)
    } else if text.is_empty() {
        Vec::new()
    } else {
        vec![text]
    };
    if let Some(cb) = cb {
        for piece in pieces {
            callback(h, cb, id, "job output", json!(piece));
        }
    }
}

fn exited<H: Host>(h: &mut H, id: u64, exit: Exit) {
    let Some(j) = h.jobs().get_mut(id) else {
        return;
    };
    // Whatever is left: a split character, a last line without "\n".
    let mut rest = Vec::new();
    let (lines, buffer) = (j.lines, j.buffer);
    for (out, cb) in [(&mut j.out, j.on_stdout), (&mut j.err, j.on_stderr)] {
        let text = out.tail.finish();
        if buffer {
            out.keep(&text);
        }
        let text = if lines {
            out.line.push_str(&text);
            std::mem::take(&mut out.line)
        } else {
            text
        };
        if let (Some(cb), false) = (cb, text.is_empty()) {
            rest.push((cb, text));
        }
    }
    for (cb, text) in rest {
        callback(h, cb, id, "job output", json!(text));
    }
    let Some(j) = h.jobs().get_mut(id) else {
        return;
    };
    j.state = if exit.error.is_some() {
        State::Failed
    } else if exit.timed_out {
        State::TimedOut
    } else if exit.cancelled {
        State::Cancelled
    } else {
        State::Exited
    };
    j.finished = Some(std::time::Instant::now());
    j.exit = Some(exit);
    let result = j.result();
    if let Some(cb) = j.on_exit {
        callback(h, cb, id, "job on_exit", result.clone());
    }
    let callbacks = h.jobs().forget_callbacks(id);
    release(h, &callbacks);
    h.jobs().retire(id);
    fire(h, "job/finished", result);
    h.changed();
}

/// Call a job callback with `(data, job)`.
fn callback<H: Host>(h: &mut H, cb: u64, id: u64, context: &str, data: Json) {
    h.call_callback_multi(cb, context, |lua| {
        Ok(vec![to_lua(lua, &data)?, handle(lua, id)?].into())
    });
}

/// Drop stored callbacks no one will call again.
fn release<H: Host>(h: &mut H, ids: &[u64]) {
    if ids.is_empty() {
        return;
    }
    let _ = h.with_api(|lua| {
        for id in ids {
            drop_callback(lua, *id)?;
        }
        Ok(())
    });
}

/// The Lua handle of job `id` (`bone.job._handle`), passed to its callbacks.
pub fn handle(lua: &Lua, id: u64) -> mlua::Result<Value> {
    let job: Table = lua.globals().get::<Table>("bone")?.get("job")?;
    match job.get::<Option<Function>>("_handle")? {
        Some(f) => f.call(id),
        None => Ok(Value::Integer(id as i64)),
    }
}

/// `bone.job.start(cmd, opts)`: the process to run.
fn job_spec(cmd: Value, opts: &Option<Table>) -> mlua::Result<Spec> {
    let command = match cmd {
        Value::String(s) => Command::Shell(s.to_str()?.to_owned()),
        Value::Table(t) => {
            let argv = t
                .sequence_values::<String>()
                .collect::<mlua::Result<Vec<_>>>()?;
            if argv.is_empty() {
                return Err(err("a job's argv list is empty"));
            }
            Command::Argv(argv)
        }
        other => {
            return Err(err(format!(
                "a job is a shell command or an argv list, not {}",
                other.type_name()
            )));
        }
    };
    let get = |k: &str| -> mlua::Result<Value> {
        opts.as_ref().map_or(Ok(Value::Nil), |o| o.get::<Value>(k))
    };
    let stdin = match get("stdin")? {
        Value::Nil | Value::Boolean(false) => Stdin::Null,
        Value::Boolean(true) => Stdin::Open,
        Value::String(s) => Stdin::Text(s.to_str()?.to_owned()),
        _ => return Err(err("job stdin is a string or true")),
    };
    let env = match get("env")? {
        Value::Nil => Vec::new(),
        Value::Table(t) => t
            .pairs::<String, String>()
            .collect::<mlua::Result<Vec<_>>>()?,
        _ => return Err(err("job env is a table of strings")),
    };
    let timeout = match get("timeout")? {
        Value::Nil => None,
        Value::Integer(ms) if ms > 0 => Some(std::time::Duration::from_millis(ms as u64)),
        Value::Number(ms) if ms > 0.0 => Some(std::time::Duration::from_millis(ms as u64)),
        _ => return Err(err("job timeout is a positive number of milliseconds")),
    };
    let pty = match get("pty")? {
        Value::Nil | Value::Boolean(false) => None,
        Value::Boolean(true) => Some((24, 80)),
        Value::Table(t) => {
            screen_size(&Some(t), (24, 80)).map(|(rows, cols, _)| Some((rows, cols)))?
        }
        _ => return Err(err("job pty is true or { width, height }")),
    };
    Ok(Spec {
        command,
        cwd: opt(opts, "cwd")?,
        env,
        stdin,
        timeout,
        pty,
    })
}

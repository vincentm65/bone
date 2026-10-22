//! The core's shell processes, for the chat on screen (`bone.processes`,
//! the tray): their snapshots as `process/changed` reports them, and a
//! terminal screen for each one drawn (`bone.processes.screen`), read from
//! the core the first time and resized to fit.

use std::collections::HashMap;

use bone_proto::methods::{
    ProcessCancel, ProcessChangedParams, ProcessRead, ProcessReadParams, ProcessRef, ProcessResize,
    ProcessResizeParams, ProcessSnapshot, ProcessesGet, SessionRef,
};
use mlua::{Lua, MultiValue, Table};

use super::{Host, args, fire, request_typed, ret};
use crate::model::Item;
use crate::term::Term;

/// What a frontend keeps of the processes of the session on screen.
#[derive(Default)]
pub struct Processes {
    /// Snapshots, by process id.
    pub list: HashMap<String, ProcessSnapshot>,
    /// Terminal views' output, by process id.
    pub terms: HashMap<String, Term>,
    /// The size each process's terminal was last given (`process/resize`),
    /// and the sizes asked for that have no reply yet.
    sizes: HashMap<String, (u16, u16)>,
    resizing: HashMap<String, (u16, u16)>,
    pub version: u64,
    /// The session they are of.
    pub session: Option<String>,
}

impl Processes {
    /// The snapshots, oldest first.
    pub fn sorted(&self) -> Vec<ProcessSnapshot> {
        let mut out: Vec<_> = self.list.values().cloned().collect();
        out.sort_by_key(|p| (p.started_at_ms, p.id.len(), p.id.clone()));
        out
    }

    pub fn any_running(&self) -> bool {
        self.list.values().any(|p| p.running)
    }
}

/// The process ops; `Err` gives the arguments back for an op that is not one.
pub fn dispatch<H: Host>(
    h: &mut H,
    lua: &Lua,
    op: &str,
    a: MultiValue,
) -> Result<mlua::Result<MultiValue>, MultiValue> {
    Ok(match op {
        "process_screen" => args::<(String, Option<Table>)>(lua, a).and_then(|(id, opts)| {
            let (rows, cols, scroll) = screen_size(&opts, (24, 80))?;
            let (lines, scroll, max) = screen(h, &id, rows, cols, scroll);
            ret(lua, screen_table(lua, &lines, scroll, max)?)
        }),
        "process_refresh" => {
            if let Some(session_id) = current_session(h) {
                request(h, session_id);
            }
            ret(lua, ())
        }
        "process_cancel" => args::<String>(lua, a).and_then(|id| {
            let Some(session_id) = current_session(h) else {
                return ret(lua, false);
            };
            request_typed::<H, ProcessCancel>(h, ProcessRef { session_id, id }, |h, r| {
                if let Err(e) = r {
                    h.error(format!("cannot cancel shell process: {e}"));
                }
            });
            ret(lua, true)
        }),
        _ => return Err(a),
    })
}

fn current_session<H: Host>(h: &mut H) -> Option<String> {
    let c = h.chat_store();
    c.chats.get(c.current)?.session_id().map(str::to_owned)
}

/// `process/changed`: a process of the session on screen changed.
pub fn changed<H: Host>(h: &mut H, p: &ProcessChangedParams) {
    let ps = h.processes();
    if ps.session.as_deref() != Some(p.session_id.as_str()) {
        return;
    }
    ps.version = ps.version.max(p.version);
    ps.list.insert(p.process.id.clone(), p.process.clone());
    // Only views being drawn keep output.
    if let (Some(c), Some(t)) = (&p.chunk, ps.terms.get_mut(&p.process.id)) {
        t.chunk(c.offset, &c.data);
    }
    h.changed();
}

/// A chat with session `session_id` came on screen: its processes.
pub fn request<H: Host>(h: &mut H, session_id: String) {
    if h.processes().session.as_deref() != Some(session_id.as_str()) {
        clear(h);
    }
    h.processes().session = Some(session_id.clone());
    let params = SessionRef {
        session_id: session_id.clone(),
    };
    request_typed::<H, ProcessesGet>(h, params, move |h, r| {
        // Older/embedded servers may not implement process snapshots; the
        // chat stays usable without them. A reply for a chat no longer on
        // screen is stale.
        let Ok(r) = r else { return };
        let ps = h.processes();
        if ps.session.as_deref() != Some(session_id.as_str()) {
            return;
        }
        ps.version = r.version;
        ps.list = r.processes.into_iter().map(|p| (p.id.clone(), p)).collect();
        let ev = serde_json::json!({ "version": ps.version, "processes": ps.sorted() });
        fire(h, "processes/changed", ev);
        h.changed();
    });
}

/// Forget the processes (a chat with no session, or a deleted one).
pub fn clear<H: Host>(h: &mut H) {
    let ps = h.processes();
    if ps.session.is_none() && ps.list.is_empty() {
        return;
    }
    *ps = Processes::default();
    fire(
        h,
        "processes/changed",
        serde_json::json!({ "version": 0, "processes": [] }),
    );
}

/// The chat on screen became `session_id` (or none): follow its processes.
pub fn follow<H: Host>(h: &mut H, session_id: Option<String>) {
    match session_id {
        Some(id) => request(h, id),
        None => clear(h),
    }
}

/// A process's terminal at `rows` × `cols`, scrolled back `scroll` rows:
/// its lines, the scroll, and how far it could scroll. Starts reading its
/// output the first time (the lines come once it arrives) and resizes a
/// running process's terminal to match.
pub fn screen<H: Host>(
    h: &mut H,
    id: &str,
    rows: u16,
    cols: u16,
    scroll: usize,
) -> (Vec<Vec<Item>>, usize, usize) {
    let ps = h.processes();
    let Some(session_id) = ps.session.clone() else {
        return (Vec::new(), 0, 0);
    };
    let running = ps.list.get(id).is_some_and(|p| p.running && p.terminal);
    let size = (cols, rows);
    if running && ps.sizes.get(id) != Some(&size) && ps.resizing.get(id) != Some(&size) {
        let pid = id.to_owned();
        ps.resizing.insert(pid.clone(), size);
        let params = ProcessResizeParams {
            session_id: session_id.clone(),
            id: pid.clone(),
            cols,
            rows,
        };
        request_typed::<H, ProcessResize>(h, params, move |h, r| {
            let ps = h.processes();
            // A newer size may have been asked for meanwhile: leave it.
            if ps.resizing.get(&pid) == Some(&size) {
                ps.resizing.remove(&pid);
            }
            if r.is_ok() {
                ps.sizes.insert(pid, size);
            }
        });
    }
    let term = h.processes().terms.entry(id.to_owned()).or_default();
    if term.stale && !term.reading {
        term.begin_read();
        let pid = id.to_owned();
        let params = ProcessReadParams {
            session_id,
            id: pid.clone(),
            from: 0,
        };
        request_typed::<H, ProcessRead>(h, params, move |h, r| {
            let failed = match (h.processes().terms.get_mut(&pid), r) {
                (Some(t), Ok(out)) => {
                    t.finish_read(out.offset, &out.data);
                    None
                }
                (Some(t), Err(e)) => {
                    t.fail_read();
                    Some(e)
                }
                (None, _) => None,
            };
            if let Some(e) = failed {
                h.error(format!("cannot read {pid}: {e}"));
            }
            h.changed();
        });
    }
    // The terminal and the theme are both the frontend's: take the term out
    // to draw it.
    let mut term = h.processes().terms.remove(id).unwrap_or_default();
    let out = term.lines(rows, cols, scroll, h.looks().theme);
    h.processes().terms.insert(id.to_owned(), term);
    out
}

/// `{ width, height, scroll }` of a terminal screen: (rows, cols, scroll),
/// the size defaulting to `size`.
pub fn screen_size(opts: &Option<Table>, size: (u16, u16)) -> mlua::Result<(u16, u16, usize)> {
    let get =
        |k: &str| -> mlua::Result<Option<i64>> { opts.as_ref().map_or(Ok(None), |t| t.get(k)) };
    Ok((
        get("height")?.map_or(size.0, |h| h.clamp(1, 500) as u16),
        get("width")?.map_or(size.1, |w| w.clamp(1, 1000) as u16),
        get("scroll")?.unwrap_or(0).max(0) as usize,
    ))
}

/// What `bone.processes.screen` and `job:screen` give: the screen's lines as
/// `{ { text, group }, … }` rows, and how far it is scrolled back.
pub fn screen_table(
    lua: &Lua,
    lines: &[Vec<Item>],
    scroll: usize,
    max: usize,
) -> mlua::Result<Table> {
    let out = lua.create_table()?;
    for (i, line) in lines.iter().enumerate() {
        let l = lua.create_table()?;
        for (j, item) in line.iter().enumerate() {
            if let Item::Text(text, group) = item {
                l.set(
                    j + 1,
                    lua.create_sequence_from([text.as_str(), group.as_str()])?,
                )?;
            }
        }
        out.set(i + 1, l)?;
    }
    let r = lua.create_table()?;
    r.set("lines", out)?;
    r.set("scroll", scroll)?;
    r.set("max", max)?;
    Ok(r)
}

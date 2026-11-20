//! What a Lua reload hands from the outgoing configuration to the incoming
//! one, in memory and never on disk: the plain tables plugins (and the
//! runtime, and the user's config) return from `bone.plugin.carry`, and the
//! running jobs the incoming configuration takes over with `bone.job.adopt`
//! (a terminal keeps its program, screen and scrollback).
//!
//! A plugin whose own Lua files changed since it loaded hands nothing over:
//! it starts fresh, and since nothing told the new state its jobs' ids,
//! they end with the outgoing state as before.
//!
//! A frontend's reload: [`collect`] while the outgoing state is current,
//! then the incoming state loads (`bone.plugin.carried` and
//! `bone.job.adopt` work while it loads and in `ready`), then [`accept`]
//! with the outgoing side's jobs (before its shutdown hooks run, so they
//! cannot stop what was taken) or [`reject`]; [`finish`] either way.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use mlua::{Function, Lua, MultiValue, Table, Value};
use serde_json::Value as Json;

use super::plugins::{plugin, release_callbacks};
use super::{Host, Owned, args, drop_callback, err, json as ret_json, opt, own, ret, store_callback};
use crate::jobs::Jobs;

/// A `bone.plugin.carry` function: its owner (`None`: the runtime or the
/// user's config), its key, and the stored callback.
pub type CarryHook = (Option<String>, String, u64);

/// A job the incoming state takes over, and its callbacks there.
pub struct Claim {
    pub id: u64,
    pub on_stdout: Option<u64>,
    pub on_stderr: Option<u64>,
    pub on_exit: Option<u64>,
}

/// Kept by the frontend outside its Lua sides, for the length of a reload.
#[derive(Default)]
pub struct Handover {
    /// The incoming state is loading.
    pub open: bool,
    /// What each carry function returned, by owner and key.
    pub carried: HashMap<(Option<String>, String), Json>,
    pub claims: Vec<Claim>,
}

/// The ops; `Err` gives the arguments back for an op that is not one.
pub(super) fn dispatch<H: Host>(
    h: &mut H,
    lua: &Lua,
    op: &str,
    a: MultiValue,
) -> Result<mlua::Result<MultiValue>, MultiValue> {
    Ok(match op {
        "plugin_carry" => args::<(String, Function)>(lua, a).and_then(|(key, f)| {
            let cb = store_callback(h, lua, f)?;
            let owner = h.owners().owner.clone();
            let hooks = &mut *h.plugin_store().carry_hooks;
            // One per owner and key: a second carry replaces the first.
            let old: Vec<u64> = (hooks.iter())
                .filter(|(o, k, _)| *o == owner && *k == key)
                .map(|(_, _, cb)| *cb)
                .collect();
            hooks.retain(|(o, k, _)| !(*o == owner && *k == key));
            hooks.push((owner, key, cb));
            for id in old {
                drop_callback(lua, id)?;
            }
            ret(lua, ())
        }),
        "plugin_carried" => args::<String>(lua, a).and_then(|key| {
            let owner = h.owners().owner.clone();
            match h.handover().carried.get(&(owner, key)).cloned() {
                Some(v) => ret_json(lua, &v),
                None => ret(lua, Value::Nil),
            }
        }),
        "job_adopt" => adopt_op(h, lua, a),
        _ => return Err(a),
    })
}

/// `bone.job.adopt(id, { on_stdout, on_stderr, on_exit })`: during a
/// reload, take over the outgoing state's job `id` once the reload is
/// accepted. False outside a reload.
fn adopt_op<H: Host>(h: &mut H, lua: &Lua, a: MultiValue) -> mlua::Result<MultiValue> {
    let (id, opts): (u64, Option<Table>) = args(lua, a)?;
    if !h.handover().open {
        return ret(lua, false);
    }
    let mut stored = Vec::new();
    let mut callback = |h: &mut H, k: &str| -> mlua::Result<Option<u64>> {
        match opt(&opts, k)? {
            None => Ok(None),
            Some(Value::Function(f)) => {
                let cb = store_callback(h, lua, f)?;
                stored.push(cb);
                Ok(Some(cb))
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
            for cb in stored {
                drop_callback(lua, cb)?;
            }
            return Err(e);
        }
    };
    h.handover().claims.push(Claim {
        id,
        on_stdout,
        on_stderr,
        on_exit,
    });
    own(h, Owned::Job(id));
    ret(lua, true)
}

/// Before the incoming state loads, with the outgoing one current: call the
/// carry functions of the runtime, the user's config, and every plugin
/// whose files are as they were when it loaded, and keep what they return.
pub fn collect<H: Host>(h: &mut H) {
    let hooks: Vec<CarryHook> = h.plugin_store().carry_hooks.clone();
    let mut same: HashMap<String, bool> = HashMap::new();
    let mut carried = HashMap::new();
    for (owner, key, cb) in hooks {
        if let Some(name) = &owner {
            let unchanged = match same.get(name) {
                Some(s) => *s,
                None => {
                    let s = plugin(h, name)
                        .is_some_and(|p| p.loaded && fingerprint(&p.dir) == p.fingerprint);
                    same.insert(name.clone(), s);
                    s
                }
            };
            if !unchanged {
                continue;
            }
        }
        let Some(v) = h.call_callback(cb, "bone.plugin.carry", |_| Ok(Value::Nil)) else {
            continue;
        };
        match bone_lua::from_lua(&v) {
            Ok(Json::Null) => {}
            Ok(v) => {
                carried.insert((owner, key), v);
            }
            Err(e) => h.lua_error("bone.plugin.carry", &e),
        }
    }
    let ho = h.handover();
    ho.carried = carried;
    ho.claims.clear();
    ho.open = true;
}

/// The reload was taken, and the incoming state is current: move each
/// claimed job out of the outgoing side's `old` jobs into this side's, with
/// its new callbacks. A claim on a job that is gone, has ended or was
/// already taken lets its callbacks go.
pub fn accept<H: Host>(h: &mut H, old: &mut Jobs) {
    let claims = std::mem::take(&mut h.handover().claims);
    let mut unused = Vec::new();
    for c in claims {
        match old.list.iter().position(|j| j.id == c.id) {
            Some(i) if old.list[i].running() => {
                let mut job = old.list.remove(i);
                job.on_stdout = c.on_stdout;
                job.on_stderr = c.on_stderr;
                job.on_exit = c.on_exit;
                h.jobs().list.push(job);
            }
            _ => unused.extend([c.on_stdout, c.on_stderr, c.on_exit].into_iter().flatten()),
        }
    }
    release_callbacks(h, &unused);
    h.changed();
}

/// The reload was not taken: the claims' callbacks went with the rejected
/// state, and the outgoing state keeps its jobs.
pub fn reject<H: Host>(h: &mut H) {
    h.handover().claims.clear();
}

/// The reload is over: nothing more is handed over.
pub fn finish<H: Host>(h: &mut H) {
    *h.handover() = Handover::default();
}

/// The Lua a frontend runs from plugin folder `dir`, as one number: every
/// `.lua` file in it but the core's `core.lua`, by path and contents.
pub fn fingerprint(dir: &Path) -> u64 {
    let mut files = Vec::new();
    lua_files(dir, dir, &mut files);
    files.sort();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for rel in &files {
        rel.hash(&mut h);
        std::fs::read(dir.join(rel)).ok().hash(&mut h);
    }
    h.finish()
}

fn lua_files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        if e.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if path.is_dir() {
            lua_files(root, &path, out);
        } else if path.extension().is_some_and(|x| x == "lua") {
            let rel = path.strip_prefix(root).unwrap_or(&path).to_owned();
            if rel != Path::new("core.lua") {
                out.push(rel);
            }
        }
    }
}

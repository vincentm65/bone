//! `bone.api.*` — the always-available Lua runtime API (Phase 6).
//!
//! Where `ctx.*` is handed to a tool/command only while it runs, `bone.api` is
//! a stable namespace usable any time (including from `init.lua` and autocmd
//! handlers). It is the `vim.api`-style surface for registering behavior,
//! remapping keys, reacting to events, and reading or writing local config.
//!
//! This module adds the event/keymap/config slices; the UI slice lives in
//! [`super::api_ui`]. All three hang off the same `bone.api` table.
//!
//! - `bone.api.autocmd(event, handler)` — alias of `bone.on`; registers a
//!   handler for any (including custom) event.
//! - `bone.api.emit(event, payload?)` — synchronously fire an event's handlers.
//! - `bone.api.keymap.set/del/get` — mutate `bone.keymap` at runtime.
//! - `bone.api.config.set/get` — mutate `bone.config` at runtime.
//!
//! Keymap and config changes mutate the live `bone.keymap` / `bone.config`
//! tables; Rust captures a snapshot of both at boot.

use mlua::{Function, Lua, Table, Value};

/// Get `bone.api`, creating it if absent (so ordering with `api_ui` is safe).
fn api_table(lua: &Lua, bone: &Table) -> mlua::Result<Table> {
    match bone.get::<Option<Table>>("api")? {
        Some(t) => Ok(t),
        None => {
            let t = lua.create_table()?;
            bone.set("api", &t)?;
            Ok(t)
        }
    }
}

/// Get a named sub-table of `bone`, creating it if absent.
fn ensure_subtable(lua: &Lua, bone: &Table, name: &str) -> mlua::Result<Table> {
    match bone.get::<Option<Table>>(name)? {
        Some(t) => Ok(t),
        None => {
            let t = lua.create_table()?;
            bone.set(name, &t)?;
            Ok(t)
        }
    }
}

/// Register `bone.api.autocmd/emit` and `bone.api.keymap.*` / `bone.api.config.*`.
pub fn setup_api(lua: &Lua, bone: &Table) -> Result<(), String> {
    let api = api_table(lua, bone).map_err(crate::util::errstr)?;

    // Ensure the live config/keymap tables exist for runtime mutation.
    ensure_subtable(lua, bone, "config").map_err(crate::util::errstr)?;
    ensure_subtable(lua, bone, "keymap").map_err(crate::util::errstr)?;

    // bone.api.autocmd = bone.on (general event registration).
    if let Some(on) = bone
        .get::<Option<Function>>("on")
        .map_err(crate::util::errstr)?
    {
        api.set("autocmd", on).map_err(crate::util::errstr)?;
    }

    // bone.api.emit(event, payload?) — synchronously invoke registered handlers.
    let emit = lua
        .create_function(|lua, (event, payload): (String, Option<Table>)| {
            let bone: Table = lua.globals().get("bone")?;
            let handlers: Option<Table> = bone.get::<Option<Table>>("_handlers")?;
            let Some(handlers) = handlers else {
                return Ok(());
            };
            let Some(arr) = handlers.get::<Option<Table>>(&*event)? else {
                return Ok(());
            };
            let payload = match payload {
                Some(p) => p,
                None => lua.create_table()?,
            };
            let ctx = lua.create_table()?;
            for h in arr.sequence_values::<Function>().flatten() {
                // Swallow handler errors so one bad autocmd can't break emit.
                if let Err(e) = h.call::<Value>((payload.clone(), ctx.clone())) {
                    eprintln!("bone-lua warn: autocmd '{event}' handler error: {e}");
                }
            }
            Ok(())
        })
        .map_err(crate::util::errstr)?;
    api.set("emit", emit).map_err(crate::util::errstr)?;

    // bone.api.submit(text) — queue a prompt for the frontend to submit, like
    // typed input. Drained between turns (or queued behind the active turn).
    let submit = lua
        .create_function(|_, text: String| {
            if !text.trim().is_empty() {
                crate::ext::inbox::push(text);
            }
            Ok(())
        })
        .map_err(crate::util::errstr)?;
    api.set("submit", submit).map_err(crate::util::errstr)?;

    // bone.api.keymap.{set,del,get}
    let keymap = lua.create_table().map_err(crate::util::errstr)?;

    let set = lua
        .create_function(|lua, (mode, key, action): (String, String, String)| {
            let bone: Table = lua.globals().get("bone")?;
            let km: Table = match bone.get::<Option<Table>>("keymap")? {
                Some(t) => t,
                None => {
                    let t = lua.create_table()?;
                    bone.set("keymap", &t)?;
                    t
                }
            };
            let mode_tbl: Table = match km.get::<Option<Table>>(&*mode)? {
                Some(t) => t,
                None => {
                    let t = lua.create_table()?;
                    km.set(&*mode, &t)?;
                    t
                }
            };
            mode_tbl.set(key, action)?;
            Ok(())
        })
        .map_err(crate::util::errstr)?;
    keymap.set("set", set).map_err(crate::util::errstr)?;

    let del = lua
        .create_function(|lua, (mode, key): (String, String)| {
            let bone: Table = lua.globals().get("bone")?;
            if let Some(km) = bone.get::<Option<Table>>("keymap")?
                && let Some(mode_tbl) = km.get::<Option<Table>>(&*mode)?
            {
                mode_tbl.set(key, Value::Nil)?;
            }
            Ok(())
        })
        .map_err(crate::util::errstr)?;
    keymap.set("del", del).map_err(crate::util::errstr)?;

    let get = lua
        .create_function(|lua, mode: String| {
            let bone: Table = lua.globals().get("bone")?;
            if let Some(km) = bone.get::<Option<Table>>("keymap")?
                && let Some(mode_tbl) = km.get::<Option<Table>>(&*mode)?
            {
                return Ok(mode_tbl);
            }
            lua.create_table()
        })
        .map_err(crate::util::errstr)?;
    keymap.set("get", get).map_err(crate::util::errstr)?;

    api.set("keymap", keymap).map_err(crate::util::errstr)?;

    // bone.api.config.{set,get}
    let config = lua.create_table().map_err(crate::util::errstr)?;

    let cset = lua
        .create_function(|lua, (key, value): (String, Value)| {
            let bone: Table = lua.globals().get("bone")?;
            let cfg: Table = match bone.get::<Option<Table>>("config")? {
                Some(t) => t,
                None => {
                    let t = lua.create_table()?;
                    bone.set("config", &t)?;
                    t
                }
            };
            cfg.set(key, value)?;
            Ok(())
        })
        .map_err(crate::util::errstr)?;
    config.set("set", cset).map_err(crate::util::errstr)?;

    let cget = lua
        .create_function(|lua, key: String| {
            let bone: Table = lua.globals().get("bone")?;
            match bone.get::<Option<Table>>("config")? {
                Some(cfg) => cfg.get::<Value>(key),
                None => Ok(Value::Nil),
            }
        })
        .map_err(crate::util::errstr)?;
    config.set("get", cget).map_err(crate::util::errstr)?;

    api.set("config", config).map_err(crate::util::errstr)?;

    Ok(())
}

#[cfg(test)]
#[path = "api_tests.rs"]
mod api_tests;

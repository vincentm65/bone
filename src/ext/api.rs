//! `bone.api.*` — the always-available Lua runtime API (Phase 6).
//!
//! Where `ctx.*` is handed to a tool/command only while it runs, `bone.api` is
//! a stable namespace usable any time (including from `init.lua` and autocmd
//! handlers). It is the `vim.api`-style surface: register behavior, remap keys,
//! react to events, read/write config — locally or, since the methods are also
//! reachable as `RuntimeCommand::ApiCall`, over RPC.
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
//! tables; Rust reads the current values via
//! [`super::ExtensionManager::keymap_snapshot_live`] /
//! [`super::ExtensionManager::config_snapshot_live`].

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
    let api = api_table(lua, bone).map_err(|e| e.to_string())?;

    // Ensure the live config/keymap tables exist for runtime mutation.
    ensure_subtable(lua, bone, "config").map_err(|e| e.to_string())?;
    ensure_subtable(lua, bone, "keymap").map_err(|e| e.to_string())?;

    // bone.api.autocmd = bone.on (general event registration).
    if let Some(on) = bone
        .get::<Option<Function>>("on")
        .map_err(|e| e.to_string())?
    {
        api.set("autocmd", on).map_err(|e| e.to_string())?;
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
        .map_err(|e| e.to_string())?;
    api.set("emit", emit).map_err(|e| e.to_string())?;

    // bone.api.keymap.{set,del,get}
    let keymap = lua.create_table().map_err(|e| e.to_string())?;

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
        .map_err(|e| e.to_string())?;
    keymap.set("set", set).map_err(|e| e.to_string())?;

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
        .map_err(|e| e.to_string())?;
    keymap.set("del", del).map_err(|e| e.to_string())?;

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
        .map_err(|e| e.to_string())?;
    keymap.set("get", get).map_err(|e| e.to_string())?;

    api.set("keymap", keymap).map_err(|e| e.to_string())?;

    // bone.api.config.{set,get}
    let config = lua.create_table().map_err(|e| e.to_string())?;

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
        .map_err(|e| e.to_string())?;
    config.set("set", cset).map_err(|e| e.to_string())?;

    let cget = lua
        .create_function(|lua, key: String| {
            let bone: Table = lua.globals().get("bone")?;
            match bone.get::<Option<Table>>("config")? {
                Some(cfg) => cfg.get::<Value>(key),
                None => Ok(Value::Nil),
            }
        })
        .map_err(|e| e.to_string())?;
    config.set("get", cget).map_err(|e| e.to_string())?;

    api.set("config", config).map_err(|e| e.to_string())?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lua_with_api() -> Lua {
        let lua = Lua::new();
        let bone = lua.create_table().unwrap();
        // bone.on + _handlers, then bone.api.*
        super::super::ops_events::setup_on(&lua, &bone).unwrap();
        setup_api(&lua, &bone).unwrap();
        lua.globals().set("bone", bone).unwrap();
        lua
    }

    #[test]
    fn autocmd_for_custom_event_fires_on_emit() {
        let lua = lua_with_api();
        lua.load(
            r#"
            _G.count = 0
            bone.api.autocmd("my_event", function(payload, ctx)
                _G.count = _G.count + (payload.n or 0)
            end)
            bone.api.autocmd("my_event", function(payload, ctx)
                _G.count = _G.count + 100
            end)
            bone.api.emit("my_event", { n = 5 })
            -- Emitting an event with no handlers is a no-op.
            bone.api.emit("nobody_listening")
        "#,
        )
        .exec()
        .unwrap();
        let count: i64 = lua.globals().get("count").unwrap();
        assert_eq!(count, 105, "both handlers ran with the payload");
    }

    #[test]
    fn keymap_set_get_del() {
        let lua = lua_with_api();
        lua.load(
            r#"
            bone.api.keymap.set("n", "ctrl+p", "open_palette")
            bone.api.keymap.set("n", "ctrl+s", "save")
            local n = bone.api.keymap.get("n")
            assert(n["ctrl+p"] == "open_palette", "binding set")
            assert(n["ctrl+s"] == "save", "second binding set")
            bone.api.keymap.del("n", "ctrl+s")
            assert(bone.api.keymap.get("n")["ctrl+s"] == nil, "binding deleted")
        "#,
        )
        .exec()
        .unwrap();

        // Rust sees the live keymap.
        let km: Table = lua
            .globals()
            .get::<Table>("bone")
            .unwrap()
            .get("keymap")
            .unwrap();
        let snap = super::super::snapshots::LuaKeymapSnapshot::from_lua_table(&lua, &km).unwrap();
        assert!(
            snap.normal
                .iter()
                .any(|b| b.key == "ctrl+p" && b.action == "open_palette")
        );
        assert!(!snap.normal.iter().any(|b| b.key == "ctrl+s"));
    }

    #[test]
    fn config_set_get() {
        let lua = lua_with_api();
        lua.load(
            r#"
            bone.api.config.set("approval_mode", "danger")
            assert(bone.api.config.get("approval_mode") == "danger")
            assert(bone.config.approval_mode == "danger", "mutates the live table")
        "#,
        )
        .exec()
        .unwrap();
    }

    /// Phase 7: the bundled `palette` menu is a Lua-defined menu that draws
    /// itself via `bone.api.ui`. Load the exact shipped file, invoke its command
    /// handler, and assert it both returns text and opened a float in the view.
    #[test]
    fn bundled_palette_menu_opens_lua_drawn_float() {
        let lua = Lua::new();
        let bone = lua.create_table().unwrap();
        super::super::ops_commands::setup_register_command(&lua, &bone).unwrap();
        super::super::ops_events::setup_on(&lua, &bone).unwrap();
        super::super::api_ui::setup_api_ui(&lua, &bone).unwrap();
        setup_api(&lua, &bone).unwrap();
        lua.globals().set("bone", bone).unwrap();

        // The exact bundled menu file shipped under defaults/lua/commands/.
        lua.load(include_str!("../../defaults/lua/commands/palette.lua"))
            .set_name("palette.lua")
            .exec()
            .unwrap();

        let handler = super::super::ops_commands::find_handler(&lua, "palette")
            .expect("palette command registered");
        let ctx = lua.create_table().unwrap();
        let ret: Table = handler.call(("", ctx)).unwrap();
        let display: String = ret.get("display").unwrap();
        assert!(display.contains("Command Palette"), "returns text fallback");

        // The menu drew itself as a float via the ViewModel UI API.
        let vm = super::super::api_ui::snapshot(&lua);
        let pc = vm
            .get("palette")
            .expect("palette float in view")
            .as_pane_content()
            .unwrap();
        assert_eq!(pc.title, "Palette");
        assert!(pc.lines.len() >= 5, "menu listed its entries");
    }
}

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
            bone.api.keymap.set("n", "ctrl+p", "open_config")
            bone.api.keymap.set("n", "ctrl+s", "save")
            local n = bone.api.keymap.get("n")
            assert(n["ctrl+p"] == "open_config", "binding set")
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
            .any(|b| b.key == "ctrl+p" && b.action == "open_config")
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

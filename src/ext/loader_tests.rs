use super::*;
use mlua::Lua;

#[test]
fn get_bone_absent_returns_none() {
    let lua = Lua::new();
    assert!(get_bone(&lua).is_none());
}

#[test]
fn get_bone_present_returns_table() {
    let lua = Lua::new();
    let bone = lua.create_table().unwrap();
    lua.globals().set("bone", bone).unwrap();
    assert!(get_bone(&lua).is_some());
}

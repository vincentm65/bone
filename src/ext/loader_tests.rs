use super::*;
use mlua::Lua;
use std::sync::{Arc, Mutex};

fn make_lua_arc(lua: Lua) -> Arc<Mutex<Lua>> {
    Arc::new(Mutex::new(lua))
}

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

#[test]
fn collect_subagent_names_no_table_returns_empty() {
    let lua = Lua::new();
    let arc = make_lua_arc(lua);
    let names = collect_subagent_names(&arc);
    assert!(names.is_empty());
}

#[test]
fn collect_subagent_names_empty_list_returns_empty() {
    let lua = Lua::new();
    let bone = lua.create_table().unwrap();
    let agents = lua.create_table().unwrap();
    bone.set("_subagents", agents).unwrap();
    lua.globals().set("bone", bone).unwrap();
    let arc = make_lua_arc(lua);
    let names = collect_subagent_names(&arc);
    assert!(names.is_empty());
}

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

#[test]
fn boot_warnings_are_routed_to_lua_log() {
    let dir = std::env::temp_dir().join(format!(
        "bone-loader-warning-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();

    log_boot_warning(&dir, "Lua tools failed: test error");

    let log = std::fs::read_to_string(dir.join("bone.log")).unwrap();
    assert!(log.contains("bone-lua [warn]: Lua tools failed: test error"));
    std::fs::remove_dir_all(dir).unwrap();
}

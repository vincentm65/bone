use super::*;

#[test]
fn legacy_register_command_alias_still_registers_handlers() {
    let lua = Lua::new();
    let bone = lua.create_table().unwrap();
    lua.globals().set("bone", bone.clone()).unwrap();
    setup_register_command(&lua, &bone).unwrap();

    lua.load("bone.register_command('legacy', function() return 'ok' end)")
        .exec()
        .unwrap();

    let handler = find_handler(&lua, "legacy").unwrap();
    assert_eq!(handler.call::<String>(()).unwrap(), "ok");
}

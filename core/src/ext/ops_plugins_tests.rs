use super::*;

#[test]
fn plugin_operations_reject_traversal_names() {
    let lua = Lua::new();
    let bone = lua.create_table().unwrap();
    bone.set("config_dir", "/tmp/bone-plugin-test").unwrap();
    bone.set("cwd", "/tmp").unwrap();
    lua.globals().set("bone", bone.clone()).unwrap();
    setup_plugin(&lua, &bone).unwrap();

    for operation in ["load", "remove", "update"] {
        let result: mlua::Result<Value> = lua
            .load(format!("return bone.plugin.{operation}('../escape')"))
            .eval();
        let error = result.expect_err("traversal name should fail").to_string();
        assert!(
            error.contains("invalid plugin name"),
            "unexpected {operation} error: {error}"
        );
    }

    let install: mlua::Result<Value> = lua.load("return bone.plugin.install('user/..')").eval();
    assert!(
        install
            .expect_err("traversal-derived install name should fail")
            .to_string()
            .contains("invalid plugin name")
    );
}

#[test]
fn plugin_names_are_single_path_components() {
    for invalid in ["", ".", "..", "../x", "x/y", r"x\y", "x\0y"] {
        assert!(
            validate_plugin_name(invalid).is_err(),
            "accepted {invalid:?}"
        );
    }
    assert!(validate_plugin_name("example.nvim").is_ok());
}

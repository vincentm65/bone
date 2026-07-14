use super::*;

fn lua_with_api() -> (Lua, SharedUi) {
    let lua = Lua::new();
    let bone = lua.create_table().unwrap();
    let shared_ui = new_shared();
    setup_api_ui(&lua, &bone, shared_ui.clone()).unwrap();
    lua.globals().set("bone", bone).unwrap();
    (lua, shared_ui)
}

#[test]
fn open_float_produces_upsert_diff_and_view_component() {
    let (lua, ui) = lua_with_api();
    let id: String = lua
        .load(
            r#"
                return bone.api.ui.open_float({
                    id = "help",
                    title = "Help",
                    lines = { "first line", "second line" },
                    width = 50, height = 12, border = true,
                })
            "#,
        )
        .eval()
        .unwrap();
    assert_eq!(id, "help");

    // The view now has the float, and a diff was recorded.
    let vm = snapshot(&ui);
    let comp = vm.get("help").expect("float in view");
    let pc = comp.as_pane_content().unwrap();
    assert_eq!(pc.title, "Help");
    assert_eq!(pc.lines.len(), 2);
    assert_eq!(pc.visible_rows, 12);

    let diffs = drain_diffs(&ui);
    assert_eq!(diffs.len(), 1);
    assert!(matches!(
        &diffs[0],
        ViewDiff::Upsert { component } if component.id() == "help"
    ));
    // Drained: a second drain is empty.
    assert!(drain_diffs(&ui).is_empty());
}

#[test]
fn set_lines_updates_existing_float() {
    let (lua, ui) = lua_with_api();
    lua.load(
        r#"
            bone.api.ui.open_float({ id = "f", lines = { "a" }, width = 20, height = 3 })
            local ok = bone.api.ui.set_lines("f", { "b", "c" })
            assert(ok == true, "set_lines should report success")
            local missing = bone.api.ui.set_lines("nope", { "x" })
            assert(missing == false, "set_lines on missing id is false")
        "#,
    )
    .exec()
    .unwrap();

    let pc = snapshot(&ui).get("f").unwrap().as_pane_content().unwrap();
    assert_eq!(pc.lines.len(), 2);
    assert!(matches!(&pc.lines[1], PaneLineSpec::Plain(s) if s == "c"));
}

#[test]
fn close_removes_and_statusline_and_highlight_apply() {
    let (lua, ui) = lua_with_api();
    lua.load(
        r##"
            bone.api.ui.open_float({ id = "f", lines = { "a" } })
            bone.api.ui.set_statusline("status", {
                { text = "ready", fg = "green", align = "right" },
            })
            bone.api.ui.set_highlight("error", "#ff0000")
            bone.api.ui.close("f")
        "##,
    )
    .exec()
    .unwrap();

    let vm = snapshot(&ui);
    assert!(vm.get("f").is_none(), "closed float removed");
    assert!(vm.get("status").is_some(), "statusline present");
    assert_eq!(
        vm.highlights.get("error").map(String::as_str),
        Some("#ff0000")
    );
}

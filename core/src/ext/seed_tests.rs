use super::*;

#[test]
fn extract_description_prefers_field_then_comment() {
    assert_eq!(
        extract_description("-- header\nregister_tool({ description = \"does a thing\" })"),
        "does a thing"
    );
    assert_eq!(
        extract_description("-- just a comment\nlocal x = 1"),
        "just a comment"
    );
    assert_eq!(extract_description("local x = 1"), "");
}

#[test]
fn catalog_extensions_are_not_bundled_defaults() {
    assert!(
        !DEFAULT_LUA_TOOLS
            .iter()
            .any(|(name, _)| *name == "task_list.lua"),
        "task_list.lua should be installed only through the catalog"
    );

    for command in ["compact.lua", "memory.lua", "usage.lua"] {
        assert!(
            !DEFAULT_LUA_COMMANDS
                .iter()
                .any(|(name, _)| *name == command),
            "{command} should not be embedded as a default command"
        );
        assert!(
            !default_command_catalog()
                .iter()
                .any(|(name, _)| *name == command),
            "{command} should not appear in the default command catalog"
        );
    }
}

#[test]
fn user_authored_commands_load_even_with_restrictive_selection() {
    assert!(
        !DEFAULT_LUA_COMMANDS
            .iter()
            .any(|(name, _)| *name == "agents.lua"),
        "agents.lua is user-owned and must not be embedded"
    );

    let dir = std::env::temp_dir().join(format!(
        "bone-user-command-test-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("agents.lua"), "loaded_user_agents = true").unwrap();

    let restrictive: HashSet<String> = HashSet::new();
    let lua = mlua::Lua::new();
    run_lua_command_files(&lua, &dir, Some(&restrictive)).unwrap();
    assert!(lua.globals().get::<bool>("loaded_user_agents").unwrap());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn allow_filter_seeds_only_named_files() {
    let dir = std::env::temp_dir().join(format!(
        "bone-seed-test-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);

    // Optional tools all moved to the catalog, so exercise the (identical)
    // seed logic against the bundled commands instead.
    // Pick the first bundled command to allow, exclude the rest.
    let first = DEFAULT_LUA_COMMANDS[0].0.to_string();
    let allow: HashSet<String> = std::iter::once(first.clone()).collect();
    seed_default_lua_commands(&dir, Some(&allow), false);

    assert!(dir.join(&first).exists(), "allowed file should be seeded");
    for (name, _) in DEFAULT_LUA_COMMANDS.iter().skip(1) {
        assert!(
            !dir.join(name).exists(),
            "non-selected file {name} should not be seeded"
        );
    }

    // None seeds everything.
    seed_default_lua_commands(&dir, None, false);
    for (name, _) in DEFAULT_LUA_COMMANDS {
        assert!(dir.join(name).exists(), "{name} should be seeded with None");
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn force_overwrites_existing_file() {
    let dir = std::env::temp_dir().join(format!(
        "bone-seed-force-test-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);

    let (first, content) = DEFAULT_LUA_COMMANDS[0];
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(first), "-- user edit with action_keys\n").unwrap();

    // Without force, an existing current-format file is left untouched.
    seed_default_lua_commands(&dir, None, false);
    assert_eq!(
        std::fs::read_to_string(dir.join(first)).unwrap(),
        "-- user edit with action_keys\n",
        "without force, existing file should be preserved"
    );

    // With force, the bundled default replaces it.
    seed_default_lua_commands(&dir, None, true);
    assert_eq!(
        std::fs::read_to_string(dir.join(first)).unwrap(),
        content,
        "force should overwrite with the bundled default"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn history_and_menu_seeds_refresh_pre_feature_copies() {
    let dir = std::env::temp_dir().join(format!(
        "bone-history-menu-seed-test-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let history = dir.join("history.lua");
    std::fs::write(&history, "-- old history helper\n").unwrap();
    assert!(should_refresh_seeded_lua(&history, "history.lua").unwrap());

    // Older history helpers that already had token counts still need the
    // candidate-first list query refresh.
    std::fs::write(&history, "function M.list() return total_token_count end\n").unwrap();
    assert!(should_refresh_seeded_lua(&history, "history.lua").unwrap());

    let menu = dir.join("menu.lua");
    std::fs::write(
        &menu,
        "local pane = require(\"ui.pane\")\n-- SELECTED_BG description_spans label_modifiers\n",
    )
    .unwrap();
    assert!(should_refresh_seeded_lua(&menu, "ui/menu.lua").unwrap());

    std::fs::write(
        &menu,
        "require(\"ui.pane\") -- SELECTED_BG description_spans label_modifiers initial_checked FULL_PREVIEW_ROWS\n",
    )
    .unwrap();
    assert!(
        should_refresh_seeded_lua(&menu, "ui/menu.lua").unwrap(),
        "menus predating content-aware preview sizing should refresh"
    );

    std::fs::write(
        &menu,
        "require(\"ui.pane\") -- SELECTED_BG description_spans label_modifiers initial_checked preview_row_budget\n",
    )
    .unwrap();
    assert!(!should_refresh_seeded_lua(&menu, "ui/menu.lua").unwrap());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn seeds_refresh_pre_namespace_registration_apis() {
    let dir = std::env::temp_dir().join(format!(
        "bone-registration-api-seed-test-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let tool = dir.join("tool.lua");
    std::fs::write(&tool, "bone.register_tool({})\n").unwrap();
    assert!(should_refresh_seeded_lua(&tool, "tool.lua").unwrap());

    let command = dir.join("command.lua");
    std::fs::write(&command, "bone.register_command('x', function() end)\n").unwrap();
    assert!(should_refresh_seeded_lua(&command, "command.lua").unwrap());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn lua_loading_continues_after_unreadable_and_invalid_files() {
    let dir = std::env::temp_dir().join(format!(
        "bone-lua-continuation-test-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("a.lua")).unwrap();
    std::fs::write(dir.join("b.lua"), "this is not valid lua (").unwrap();
    std::fs::write(dir.join("c.lua"), "loaded_after_failure = true").unwrap();

    let lua = mlua::Lua::new();
    run_lua_files_filtered(&lua, &dir, |_| true).unwrap();
    assert!(lua.globals().get::<bool>("loaded_after_failure").unwrap());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unreadable_seed_target_is_preserved() {
    let dir = std::env::temp_dir().join(format!(
        "bone-unreadable-seed-test-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let target = dir.join("locked.lua");
    std::fs::create_dir_all(&target).unwrap();

    seed_default_lua(&dir, &[("locked.lua", "replacement")], None, false);
    assert!(
        target.is_dir(),
        "unreadable existing target was not preserved"
    );

    seed_default_lua(&dir, &[("locked.lua", "replacement")], None, true);
    assert!(target.is_dir(), "force replaced a directory with a file");

    let _ = std::fs::remove_dir_all(&dir);
}

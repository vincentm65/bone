use super::*;

#[test]
fn canonical_disabled_tools_are_excluded() {
    let _guard = crate::util::test_env_lock();
    let previous = std::env::var_os("BONE_DIR");
    let dir = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("BONE_DIR", dir.path()) };

    let config =
        crate::config::store::ConfigStore::new(crate::ext::ExtensionManager::unloaded()).unwrap();
    let revision = config.snapshot().revision;
    config
        .set_enabled("tools", "cron", false, revision)
        .unwrap();

    assert_eq!(
        configured_tool_names(&config, vec!["shell".into(), "cron".into()]),
        vec!["shell"]
    );

    unsafe {
        match previous {
            Some(value) => std::env::set_var("BONE_DIR", value),
            None => std::env::remove_var("BONE_DIR"),
        }
    }
}

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
    std::fs::write(dir.join(first), "-- user edit with canonical-config-v7\n").unwrap();

    // Without force, an existing current-format file is left untouched.
    seed_default_lua_commands(&dir, None, false);
    assert_eq!(
        std::fs::read_to_string(dir.join(first)).unwrap(),
        "-- user edit with canonical-config-v7\n",
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
fn bundled_ui_seeds_refresh_pre_feature_copies() {
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
        "require(\"ui.pane\") -- SELECTED_BG description_spans label_modifiers initial_checked preview_row_budget multi-space-toggle-v2\n",
    )
    .unwrap();
    assert!(!should_refresh_seeded_lua(&menu, "ui/menu.lua").unwrap());

    let config = dir.join("config.lua");
    std::fs::write(&config, "-- old config command\n").unwrap();
    assert!(!should_refresh_seeded_lua(&config, "config.lua").unwrap());
    std::fs::write(&config, "-- canonical-config-v2\n").unwrap();
    assert!(!should_refresh_seeded_lua(&config, "config.lua").unwrap());
    std::fs::write(&config, "-- canonical-config-v3\n").unwrap();
    assert!(!should_refresh_seeded_lua(&config, "config.lua").unwrap());
    std::fs::write(&config, "-- canonical-config-v4\n").unwrap();
    assert!(!should_refresh_seeded_lua(&config, "config.lua").unwrap());
    std::fs::write(&config, "-- canonical-config-v5\n").unwrap();
    assert!(!should_refresh_seeded_lua(&config, "config.lua").unwrap());
    std::fs::write(&config, "-- canonical-config-v6\n-- user customization\n").unwrap();
    assert!(
        !should_refresh_seeded_lua(&config, "config.lua").unwrap(),
        "customized v6 config must be preserved"
    );
    std::fs::write(&config, "-- canonical-config-v7\n").unwrap();
    assert!(!should_refresh_seeded_lua(&config, "config.lua").unwrap());
    // v8 is now a legacy seed too: the pristine digest (see
    // CANONICAL_CONFIG_V8_SHA256) refreshes, but any edited v8 the user has
    // changed — even one keeping the marker — is preserved.
    std::fs::write(&config, "-- canonical-config-v8\n").unwrap();
    assert!(!should_refresh_seeded_lua(&config, "config.lua").unwrap());
    std::fs::write(&config, "-- canonical-config-v8\n-- user customization\n").unwrap();
    assert!(
        !should_refresh_seeded_lua(&config, "config.lua").unwrap(),
        "edited v8 config must be preserved"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bundled_seed_names_use_forward_slashes() {
    for (name, _) in DEFAULT_LUA_LIBS
        .iter()
        .chain(DEFAULT_LUA_TOOLS)
        .chain(DEFAULT_LUA_COMMANDS)
    {
        assert!(
            !name.contains('\\'),
            "bundled seed name {name:?} must use `/`, because refresh rules match literal forward-slash paths"
        );
    }

    // Regression: the Windows build used to emit `ui\menu.lua`, so the
    // `name == "ui/menu.lua"` refresh rule below never fired and the seeded
    // menu drifted forever.
    let (name, content) = DEFAULT_LUA_LIBS
        .iter()
        .find(|(name, _)| *name == "ui/menu.lua")
        .expect("bundled libs include ui/menu.lua");

    let dir = std::env::temp_dir().join(format!(
        "bone-seed-name-separator-test-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let menu = dir.join("ui").join("menu.lua");
    std::fs::create_dir_all(menu.parent().unwrap()).unwrap();

    // A pristine copy of the current bundled menu is already up to date.
    std::fs::write(&menu, content).unwrap();
    assert!(
        !should_refresh_seeded_lua(&menu, name).unwrap(),
        "bundled ui/menu.lua should satisfy its own refresh rule"
    );

    // A pre-pane-migration copy must still be refreshed under that name.
    std::fs::write(&menu, "-- old menu helper\n").unwrap();
    assert!(should_refresh_seeded_lua(&menu, name).unwrap());

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
    let error = run_lua_files_filtered(&lua, &dir, |_| true).unwrap_err();
    assert!(error.contains("failed to read"));
    assert!(error.contains("error executing"));
    assert!(lua.globals().get::<bool>("loaded_after_failure").unwrap());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn settings_owners_are_scoped_and_failed_files_roll_back_all_pages() {
    let dir = std::env::temp_dir().join(format!(
        "bone-settings-owner-test-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("a.lua"),
        r#"
        bone.settings.define("original", {
          title = "Original",
          fields = { enabled = { label = "Enabled", type = "bool", default = true } },
        })
        "#,
    )
    .unwrap();
    std::fs::write(
        dir.join("b.lua"),
        r#"
        bone.settings.define("transient_one", {
          title = "One",
          fields = { value = { label = "Value", type = "string", default = "one" } },
        })
        bone.settings.define("transient_two", {
          title = "Two",
          fields = { value = { label = "Value", type = "string", default = "two" } },
        })
        error("fail after registration")
        "#,
    )
    .unwrap();
    std::fs::write(
        dir.join("c.lua"),
        r#"
        assert(not pcall(bone.settings.define, "original", {
          title = "Collision",
          fields = { value = { label = "Value", type = "string", default = "bad" } },
        }))
        bone.settings.define("survivor", {
          title = "Survivor",
          fields = { value = { label = "Value", type = "string", default = "ok" } },
        })
        "#,
    )
    .unwrap();

    let lua = mlua::Lua::new();
    let bone = lua.create_table().unwrap();
    super::ops_events::setup_on(&lua, &bone).unwrap();
    let registry = std::sync::Arc::new(std::sync::RwLock::new(Default::default()));
    super::api::setup_api(
        &lua,
        &bone,
        std::sync::Arc::new(std::sync::Mutex::new(
            crate::config::settings::Settings::defaults(),
        )),
        std::sync::Arc::clone(&registry),
        dir.join("config.yaml"),
        super::api_ui::new_shared(),
    )
    .unwrap();
    lua.globals().set("bone", bone).unwrap();

    let error = run_lua_files_filtered(&lua, &dir, |_| true).unwrap_err();
    assert!(error.contains("fail after registration"));

    let pages = registry.read().unwrap().pages();
    assert_eq!(
        pages
            .iter()
            .map(|page| page.namespace.as_str())
            .collect::<Vec<_>>(),
        vec!["original", "survivor"]
    );
    assert_eq!(pages[0].owner, dir.join("a.lua").to_string_lossy());
    assert_eq!(pages[1].owner, dir.join("c.lua").to_string_lossy());
    let bone: mlua::Table = lua.globals().get("bone").unwrap();
    assert!(
        bone.get::<Option<String>>("_settings_owner")
            .unwrap()
            .is_none()
    );

    let _ = std::fs::remove_dir_all(dir);
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

// ── Plugin package loading (Model A) ────────────────────────────────────────

/// Build a Lua VM with a `bone` global exposing `bone.tool.register`, matching
/// what `ops_tools::setup_register_tool` installs at boot.
fn plugin_test_lua() -> mlua::Lua {
    let lua = mlua::Lua::new();
    let bone = lua.create_table().unwrap();
    super::ops_tools::setup_register_tool(&lua, &bone).unwrap();
    lua.globals().set("bone", bone).unwrap();
    lua
}

/// The `plugin` field stamped on each `bone._tools` entry (or `None`).
fn registered_tool_plugins(lua: &mlua::Lua) -> Vec<Option<String>> {
    let bone: mlua::Table = lua.globals().get("bone").unwrap();
    let tools: mlua::Table = bone.get("_tools").unwrap();
    tools
        .sequence_values::<mlua::Table>()
        .map(|entry| entry.unwrap().get::<Option<String>>("plugin").unwrap())
        .collect()
}

const ALPHA_TOOL_LUA: &str = r#"bone.tool.register({ name = "alpha_tool", description = "d", parameters = {}, execute = function() return "ok" end })"#;

#[test]
fn plugin_entry_points_load_and_stamp_owner() {
    let dir = tempfile::tempdir().unwrap();
    let plugins = dir.path().join("plugins");
    std::fs::create_dir_all(plugins.join("alpha")).unwrap();
    std::fs::write(plugins.join("alpha/init.lua"), ALPHA_TOOL_LUA).unwrap();

    let lua = plugin_test_lua();
    run_lua_plugin_files(&lua, &plugins, None).unwrap();

    assert_eq!(
        registered_tool_plugins(&lua),
        vec![Some("alpha".to_string())]
    );
}

#[test]
fn disabled_plugins_are_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let plugins = dir.path().join("plugins");
    std::fs::create_dir_all(plugins.join("alpha")).unwrap();
    std::fs::write(plugins.join("alpha/init.lua"), ALPHA_TOOL_LUA).unwrap();

    let lua = plugin_test_lua();
    let disabled: std::collections::HashSet<String> = ["alpha".to_string()].into_iter().collect();
    run_lua_plugin_files(&lua, &plugins, Some(&disabled)).unwrap();

    assert!(registered_tool_plugins(&lua).is_empty());
}

#[test]
fn plugin_dir_without_init_lua_is_inert() {
    let dir = tempfile::tempdir().unwrap();
    let plugins = dir.path().join("plugins");
    std::fs::create_dir_all(plugins.join("empty")).unwrap();
    std::fs::write(plugins.join("empty/readme.md"), "no init here").unwrap();

    let lua = plugin_test_lua();
    run_lua_plugin_files(&lua, &plugins, None).unwrap();
    assert!(registered_tool_plugins(&lua).is_empty());
}

#[test]
fn missing_plugins_dir_is_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let lua = plugin_test_lua();
    run_lua_plugin_files(&lua, &dir.path().join("nope"), None).unwrap();
    assert!(registered_tool_plugins(&lua).is_empty());
}

#[test]
fn plugin_errors_are_attributed_to_the_plugin() {
    let dir = tempfile::tempdir().unwrap();
    let plugins = dir.path().join("plugins");
    std::fs::create_dir_all(plugins.join("broken")).unwrap();
    std::fs::write(plugins.join("broken/init.lua"), "this is not valid lua (").unwrap();

    let lua = plugin_test_lua();
    let error = run_lua_plugin_files(&lua, &plugins, None).unwrap_err();
    assert!(
        error.contains("plugin 'broken'"),
        "unexpected error: {error}"
    );
}

#[test]
fn plugin_owner_global_is_reset_after_load() {
    let dir = tempfile::tempdir().unwrap();
    let plugins = dir.path().join("plugins");
    std::fs::create_dir_all(plugins.join("alpha")).unwrap();
    std::fs::write(plugins.join("alpha/init.lua"), ALPHA_TOOL_LUA).unwrap();

    let lua = plugin_test_lua();
    run_lua_plugin_files(&lua, &plugins, None).unwrap();

    let bone: mlua::Table = lua.globals().get("bone").unwrap();
    assert!(
        bone.get::<Option<String>>("_plugin_owner")
            .unwrap()
            .is_none(),
        "_plugin_owner leaked past plugin execution"
    );
}

#[test]
fn plugin_package_path_enables_in_package_requires() {
    let dir = tempfile::tempdir().unwrap();
    let plugins = dir.path().join("plugins");
    std::fs::create_dir_all(plugins.join("alpha/lib")).unwrap();
    std::fs::write(plugins.join("alpha/lib/helper.lua"), "return { answer = 42 }\n").unwrap();
    std::fs::write(
        plugins.join("alpha/init.lua"),
        r#"
        local helper = require("lib.helper")
        bone.tool.register({ name = "alpha_tool", description = "d", parameters = {}, execute = function() return helper.answer end })
        "#,
    )
    .unwrap();
    std::fs::create_dir_all(plugins.join("inactive")).unwrap();
    std::fs::write(plugins.join("inactive/init.lua"), ALPHA_TOOL_LUA).unwrap();

    let disabled: std::collections::HashSet<String> = ["inactive".to_string()]
        .into_iter()
        .collect();
    let lua = plugin_test_lua();
    run_lua_plugin_files(&lua, &plugins, Some(&disabled)).unwrap();

    let alpha = plugins.join("alpha").to_string_lossy().into_owned();
    let patterns: Vec<String> = vec![
        format!("{alpha}/?.lua"),
        format!("{alpha}/lib/?.lua"),
        format!("{alpha}/lib/?/init.lua"),
    ];
    let package_path = || {
        let package: mlua::Table = lua.globals().get("package").unwrap();
        package.get::<String>("path").unwrap()
    };
    let path = package_path();
    for pattern in &patterns {
        let count = path.split(';').filter(|p| *p == pattern.as_str()).count();
        assert_eq!(
            count, 1,
            "pattern {pattern} expected exactly once in package.path: {path}"
        );
    }
    let inactive = plugins.join("inactive").to_string_lossy().into_owned();
    assert!(
        !path.contains(&inactive),
        "a disabled plugin must gain no require path: {path}"
    );

    // The require inside init.lua resolved against the plugin's own lib/.
    assert_eq!(registered_tool_plugins(&lua), vec![Some("alpha".to_string())]);
    let answer: i64 = lua.load(r#"return require("lib.helper").answer"#).eval().unwrap();
    assert_eq!(answer, 42);

    // A hot reload re-runs the packages but must not duplicate search paths.
    run_lua_plugin_files(&lua, &plugins, Some(&disabled)).unwrap();
    let path = package_path();
    for pattern in &patterns {
        let count = path.split(';').filter(|p| *p == pattern.as_str()).count();
        assert_eq!(
            count, 1,
            "pattern {pattern} duplicated after reload: {path}"
        );
    }
}

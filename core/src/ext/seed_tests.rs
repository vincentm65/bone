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
fn catalog_extensions_are_not_bundled_defaults() {
    let bundled = |key: &str| DEFAULT_LUA_PLUGINS.iter().any(|(name, _)| *name == key);
    assert!(
        !bundled("task_list/init.lua"),
        "task_list should be installed only through the catalog"
    );

    for stem in ["compact", "memory", "usage"] {
        assert!(
            !bundled(&format!("{stem}/init.lua")),
            "{stem} should not be embedded as a bundled default"
        );
    }
}

#[test]
fn user_authored_plugins_load_even_with_restrictive_selection() {
    assert!(
        !DEFAULT_LUA_PLUGINS
            .iter()
            .any(|(name, _)| name.starts_with("agents/")),
        "agents is user-owned and must not be embedded"
    );

    let dir = std::env::temp_dir().join(format!(
        "bone-user-command-test-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("agents")).unwrap();
    std::fs::write(dir.join("agents/init.lua"), "loaded_user_agents = true").unwrap();

    let restrictive: HashSet<String> = HashSet::new();
    let lua = mlua::Lua::new();
    run_lua_plugin_files(&lua, &dir, Some(&restrictive)).unwrap();
    assert!(lua.globals().get::<bool>("loaded_user_agents").unwrap());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn optional_selection_does_not_affect_core_seeding() {
    let dir = std::env::temp_dir().join(format!(
        "bone-seed-split-test-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);

    // An empty selection (everything deselected) still materializes the whole
    // Bone-owned core package.
    let allow: HashSet<String> = HashSet::new();
    seed_default_lua_core(&dir.join("core"), false);
    seed_default_lua_plugins(&dir.join("plugins"), Some(&allow), false);

    for (name, _) in DEFAULT_LUA_CORE {
        assert!(
            dir.join("core").join(name).exists(),
            "core {name} is always seeded regardless of optional selection"
        );
    }
    // Core is a separate embedded table and never an optional plugin package.
    assert!(
        !DEFAULT_LUA_PLUGINS
            .iter()
            .any(|(name, _)| bundled_plugin_of(name) == BUNDLED_CORE_DIR),
        "core must not appear among the optional bundled plugins"
    );
    assert!(
        !dir.join("plugins").join(BUNDLED_CORE_DIR).exists(),
        "core must not be seeded under lua/plugins"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn seed_default_lua_core_seeds_every_core_file() {
    let dir = std::env::temp_dir().join(format!(
        "bone-seed-core-test-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);

    seed_default_lua_core(&dir, false);
    for (name, _) in DEFAULT_LUA_CORE {
        assert!(dir.join(name).exists(), "core {name} should be seeded");
    }
    assert!(dir.join("init.lua").exists());

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

    let &(first, content) = DEFAULT_LUA_CORE
        .iter()
        .find(|(name, _)| *name == "init.lua")
        .expect("core init.lua is bundled");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(first), "-- user edit with canonical-config-v7\n").unwrap();

    // Without force, an existing current-format file is left untouched.
    seed_default_lua_core(&dir, false);
    assert_eq!(
        std::fs::read_to_string(dir.join(first)).unwrap(),
        "-- user edit with canonical-config-v7\n",
        "without force, existing file should be preserved"
    );

    // With force, the bundled default replaces it.
    seed_default_lua_core(&dir, true);
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
    assert!(should_refresh_seeded_lua(&history, "lib/history.lua").unwrap());

    // Older history helpers that already had token counts still need the
    // candidate-first list query refresh.
    std::fs::write(&history, "function M.list() return total_token_count end\n").unwrap();
    assert!(should_refresh_seeded_lua(&history, "lib/history.lua").unwrap());

    let menu = dir.join("menu.lua");
    std::fs::write(
        &menu,
        "local pane = require(\"ui.pane\")\n-- SELECTED_BG description_spans label_modifiers\n",
    )
    .unwrap();
    assert!(should_refresh_seeded_lua(&menu, "lib/ui/menu.lua").unwrap());

    std::fs::write(
        &menu,
        "require(\"ui.pane\") -- SELECTED_BG description_spans label_modifiers initial_checked FULL_PREVIEW_ROWS\n",
    )
    .unwrap();
    assert!(
        should_refresh_seeded_lua(&menu, "lib/ui/menu.lua").unwrap(),
        "menus predating content-aware preview sizing should refresh"
    );

    std::fs::write(
        &menu,
        "require(\"ui.pane\") -- SELECTED_BG description_spans label_modifiers initial_checked preview_row_budget multi-space-toggle-v2\n",
    )
    .unwrap();
    assert!(!should_refresh_seeded_lua(&menu, "lib/ui/menu.lua").unwrap());

    let config = dir.join("config.lua");
    std::fs::write(&config, "-- old config command\n").unwrap();
    assert!(!should_refresh_seeded_lua(&config, "init.lua").unwrap());
    std::fs::write(&config, "-- canonical-config-v2\n").unwrap();
    assert!(!should_refresh_seeded_lua(&config, "init.lua").unwrap());
    std::fs::write(&config, "-- canonical-config-v3\n").unwrap();
    assert!(!should_refresh_seeded_lua(&config, "init.lua").unwrap());
    std::fs::write(&config, "-- canonical-config-v4\n").unwrap();
    assert!(!should_refresh_seeded_lua(&config, "init.lua").unwrap());
    std::fs::write(&config, "-- canonical-config-v5\n").unwrap();
    assert!(!should_refresh_seeded_lua(&config, "init.lua").unwrap());
    std::fs::write(&config, "-- canonical-config-v6\n-- user customization\n").unwrap();
    assert!(
        !should_refresh_seeded_lua(&config, "init.lua").unwrap(),
        "customized v6 config must be preserved"
    );
    std::fs::write(&config, "-- canonical-config-v7\n").unwrap();
    assert!(!should_refresh_seeded_lua(&config, "init.lua").unwrap());
    // v8 is now a legacy seed too: the pristine digest (see
    // CANONICAL_CONFIG_V8_SHA256) refreshes, but any edited v8 the user has
    // changed — even one keeping the marker — is preserved.
    std::fs::write(&config, "-- canonical-config-v8\n").unwrap();
    assert!(!should_refresh_seeded_lua(&config, "init.lua").unwrap());
    std::fs::write(&config, "-- canonical-config-v8\n-- user customization\n").unwrap();
    assert!(
        !should_refresh_seeded_lua(&config, "init.lua").unwrap(),
        "edited v8 config must be preserved"
    );
    // v9: only the exact pre-88ba547 seed digest refreshes; the marker alone
    // (including the current bundled file) never does.
    std::fs::write(&config, "-- canonical-config-v9\n").unwrap();
    assert!(!should_refresh_seeded_lua(&config, "init.lua").unwrap());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bundled_seed_names_use_forward_slashes() {
    for (name, _) in DEFAULT_LUA_CORE {
        assert!(
            !name.contains('\\'),
            "bundled seed name {name:?} must use `/`, because refresh rules match literal forward-slash paths"
        );
    }

    // Regression: the Windows build used to emit `ui\menu.lua`, so the
    // `name == "lib/ui/menu.lua"` refresh rule below never fired and the
    // seeded menu drifted forever.
    let (name, content) = DEFAULT_LUA_CORE
        .iter()
        .find(|(name, _)| *name == "lib/ui/menu.lua")
        .expect("bundled core includes lib/ui/menu.lua");

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

    // An unreadable source path reports a read error rather than panicking.
    let lua = mlua::Lua::new();
    let error = exec_lua_file(&lua, &dir.join("missing.lua"), "missing", None).unwrap_err();
    assert!(
        error.contains("failed to read"),
        "unexpected error: {error}"
    );

    // A broken plugin does not stop later plugins from loading.
    let plugins = dir.join("plugins");
    std::fs::create_dir_all(plugins.join("b")).unwrap();
    std::fs::write(plugins.join("b/init.lua"), "this is not valid lua (").unwrap();
    std::fs::create_dir_all(plugins.join("c")).unwrap();
    std::fs::write(plugins.join("c/init.lua"), "loaded_after_failure = true").unwrap();

    let error = run_lua_plugin_files(&lua, &plugins, None).unwrap_err();
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
    let plugins = dir.join("plugins");
    for name in ["a", "b", "c"] {
        std::fs::create_dir_all(plugins.join(name)).unwrap();
    }
    std::fs::write(
        plugins.join("a/init.lua"),
        r#"
        bone.settings.define("original", {
          title = "Original",
          fields = { enabled = { label = "Enabled", type = "bool", default = true } },
        })
        "#,
    )
    .unwrap();
    std::fs::write(
        plugins.join("b/init.lua"),
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
        plugins.join("c/init.lua"),
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

    let error = run_lua_plugin_files(&lua, &plugins, None).unwrap_err();
    assert!(error.contains("fail after registration"));

    let pages = registry.read().unwrap().pages();
    assert_eq!(
        pages
            .iter()
            .map(|page| page.namespace.as_str())
            .collect::<Vec<_>>(),
        vec!["original", "survivor"]
    );
    assert_eq!(pages[0].owner, plugins.join("a/init.lua").to_string_lossy());
    assert_eq!(pages[1].owner, plugins.join("c/init.lua").to_string_lossy());
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

    seed_default_lua(&dir, &[("locked.lua", "replacement")], |_| true, false);
    assert!(
        target.is_dir(),
        "unreadable existing target was not preserved"
    );

    seed_default_lua(&dir, &[("locked.lua", "replacement")], |_| true, true);
    assert!(target.is_dir(), "force replaced a directory with a file");
    let _ = std::fs::remove_dir_all(&dir);
}

// ── Flat → plugin-package migration ─────────────────────────────────────────

/// Run `f` against a fresh `lua/` tree with `BONE_DIR` pointed at the temp root,
/// so migration notices never touch the real config directory. `f` receives the
/// `lua/` directory and the config root.
fn with_migration_root(f: impl FnOnce(&Path, &Path)) {
    let _guard = crate::util::test_env_lock();
    let previous = std::env::var_os("BONE_DIR");
    let root = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("BONE_DIR", root.path()) };
    let lua = root.path().join("lua");
    std::fs::create_dir_all(&lua).unwrap();
    f(&lua, root.path());
    unsafe {
        match previous {
            Some(value) => std::env::set_var("BONE_DIR", value),
            None => std::env::remove_var("BONE_DIR"),
        }
    }
}

#[test]
fn migrate_flat_files_become_packages_and_prune_dirs() {
    with_migration_root(|lua, _root| {
        std::fs::create_dir_all(lua.join("tools")).unwrap();
        std::fs::create_dir_all(lua.join("commands")).unwrap();
        std::fs::write(lua.join("tools/weather.lua"), "weather body").unwrap();
        std::fs::write(lua.join("commands/agents.lua"), "agents body").unwrap();

        migrate_flat_lua_extensions(lua);

        assert_eq!(
            std::fs::read_to_string(lua.join("plugins/weather/init.lua")).unwrap(),
            "weather body"
        );
        assert_eq!(
            std::fs::read_to_string(lua.join("plugins/agents/init.lua")).unwrap(),
            "agents body"
        );
        assert!(
            !lua.join("tools").exists(),
            "emptied tools dir should be pruned"
        );
        assert!(
            !lua.join("commands").exists(),
            "emptied commands dir should be pruned"
        );
    });
}

#[test]
fn migrate_deletes_flat_file_identical_to_destination() {
    with_migration_root(|lua, _root| {
        std::fs::create_dir_all(lua.join("plugins/weather")).unwrap();
        std::fs::write(lua.join("plugins/weather/init.lua"), "same").unwrap();
        std::fs::create_dir_all(lua.join("tools")).unwrap();
        std::fs::write(lua.join("tools/weather.lua"), "same").unwrap();

        migrate_flat_lua_extensions(lua);

        assert!(!lua.join("tools/weather.lua").exists());
        assert_eq!(
            std::fs::read_to_string(lua.join("plugins/weather/init.lua")).unwrap(),
            "same"
        );
    });
}

#[test]
fn migrate_preserves_conflicting_flat_file_as_backup() {
    with_migration_root(|lua, _root| {
        std::fs::create_dir_all(lua.join("plugins/weather")).unwrap();
        std::fs::write(lua.join("plugins/weather/init.lua"), "bundled").unwrap();
        std::fs::create_dir_all(lua.join("tools")).unwrap();
        std::fs::write(lua.join("tools/weather.lua"), "user edit").unwrap();

        migrate_flat_lua_extensions(lua);

        assert_eq!(
            std::fs::read_to_string(lua.join("plugins/weather/init.lua")).unwrap(),
            "bundled"
        );
        assert!(!lua.join("tools/weather.lua").exists());
        assert_eq!(
            std::fs::read_to_string(lua.join("tools/weather.lua.bundled-backup")).unwrap(),
            "user edit"
        );
    });
}

#[test]
fn migrate_remaps_pristine_config_command_into_bundled_core() {
    with_migration_root(|lua, _root| {
        let (_, bundled) = DEFAULT_LUA_CORE
            .iter()
            .find(|(name, _)| *name == "init.lua")
            .expect("core is bundled");
        std::fs::create_dir_all(lua.join("commands")).unwrap();
        std::fs::write(lua.join("commands/config.lua"), bundled).unwrap();

        migrate_flat_lua_extensions(lua);

        assert!(!lua.join("commands/config.lua").exists());
        assert!(
            !lua.join("plugins/config").exists(),
            "config must never migrate into a shadow package"
        );
    });
}

#[test]
fn migrate_sets_aside_customized_config_command() {
    with_migration_root(|lua, _root| {
        std::fs::create_dir_all(lua.join("commands")).unwrap();
        std::fs::write(lua.join("commands/config.lua"), "-- my own config\n").unwrap();

        migrate_flat_lua_extensions(lua);

        assert!(!lua.join("commands/config.lua").exists());
        assert!(!lua.join("plugins/config").exists());
        assert_eq!(
            std::fs::read_to_string(lua.join("commands/config.lua.bundled-backup")).unwrap(),
            "-- my own config\n"
        );
    });
}

#[test]
fn migrate_relocates_legacy_lib_modules() {
    with_migration_root(|lua, _root| {
        // A pristine copy of a bundled module is deleted; an edited one is set
        // aside so it can never shadow the package copy.
        let (_, exact) = DEFAULT_LUA_CORE
            .iter()
            .find(|(name, _)| *name == "lib/history.lua")
            .expect("core lib/history.lua is bundled");
        std::fs::create_dir_all(lua.join("lib/ui")).unwrap();
        std::fs::write(lua.join("lib/history.lua"), exact).unwrap();
        std::fs::write(lua.join("lib/ui/menu.lua"), "-- customized menu\n").unwrap();

        migrate_flat_lua_extensions(lua);

        assert!(!lua.join("lib/history.lua").exists());
        assert!(!lua.join("lib/ui/menu.lua").exists());
        assert_eq!(
            std::fs::read_to_string(lua.join("lib/ui/menu.lua.bundled-backup")).unwrap(),
            "-- customized menu\n"
        );
        assert!(
            lua.join("lib/ui").is_dir(),
            "a dir holding a backup is retained"
        );
    });
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

// ── Built-in core loading ────────────────────────────────────────────────────

const CORE_TOOL_LUA: &str = r#"bone.tool.register({ name = "core_tool", description = "d", parameters = {}, execute = function() return "ok" end })"#;

#[test]
fn core_init_loads_without_owner() {
    let dir = tempfile::tempdir().unwrap();
    let core = dir.path().join("core");
    std::fs::create_dir_all(&core).unwrap();
    std::fs::write(core.join("init.lua"), CORE_TOOL_LUA).unwrap();

    let lua = plugin_test_lua();
    run_lua_core_file(&lua, &core).unwrap();

    // Core registrations carry no plugin owner.
    assert_eq!(registered_tool_plugins(&lua), vec![None]);
    let bone: mlua::Table = lua.globals().get("bone").unwrap();
    assert!(
        bone.get::<Option<String>>("_plugin_owner")
            .unwrap()
            .is_none(),
        "core must leave _plugin_owner unset"
    );
}

#[test]
fn legacy_plugins_core_dir_is_ignored_and_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let plugins = dir.path().join("plugins");
    // A legacy `lua/plugins/core` package (pre-split layout) plus a normal one.
    std::fs::create_dir_all(plugins.join("core")).unwrap();
    std::fs::write(plugins.join("core/init.lua"), CORE_TOOL_LUA).unwrap();
    std::fs::create_dir_all(plugins.join("alpha")).unwrap();
    std::fs::write(plugins.join("alpha/init.lua"), ALPHA_TOOL_LUA).unwrap();

    // A persisted disabled set may still contain "core"; the legacy dir is
    // ignored (never executed) and a normal plugin keeps loading.
    let disabled: std::collections::HashSet<String> = ["core".to_string()].into_iter().collect();
    let lua = plugin_test_lua();
    run_lua_plugin_files(&lua, &plugins, Some(&disabled)).unwrap();

    assert_eq!(
        registered_tool_plugins(&lua),
        vec![Some("alpha".to_string())],
        "the legacy plugins/core dir must never execute"
    );
    assert!(
        plugins.join("core/init.lua").exists(),
        "the legacy plugins/core dir must not be deleted"
    );
}

/// A bundled core file that no refresh rule touches, for migration tests.
fn banner_core_file() -> (&'static str, &'static str) {
    *DEFAULT_LUA_CORE
        .iter()
        .find(|(name, _)| *name == "lib/banner.lua")
        .expect("bundled lib/banner.lua")
}

fn write_legacy_core(lua: &std::path::Path, name: &str, content: &str) {
    let path = lua.join("plugins/core").join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

#[test]
fn migrate_legacy_plugins_core_moves_missing_files_and_prunes() {
    let dir = tempfile::tempdir().unwrap();
    let lua = dir.path();
    write_legacy_core(lua, "init.lua", CORE_TOOL_LUA);
    write_legacy_core(lua, "lib/custom.lua", "return {}\n");

    migrate_legacy_plugins_core(lua);
    seed_default_lua_core(&lua.join("core"), false);

    assert_eq!(
        std::fs::read_to_string(lua.join("core/init.lua")).unwrap(),
        CORE_TOOL_LUA,
        "a customized legacy init.lua must survive migration and seeding"
    );
    assert_eq!(
        std::fs::read_to_string(lua.join("core/lib/custom.lua")).unwrap(),
        "return {}\n"
    );
    assert!(!lua.join("plugins/core").exists(), "empty legacy dir must be pruned");
    for (name, _) in DEFAULT_LUA_CORE {
        assert!(lua.join("core").join(name).exists(), "{name} was not seeded");
    }
}

#[test]
fn migrate_legacy_plugins_core_deletes_identical_copy() {
    let dir = tempfile::tempdir().unwrap();
    let lua = dir.path();
    let (name, bundled) = banner_core_file();
    seed_default_lua_core(&lua.join("core"), false);
    write_legacy_core(lua, name, bundled);

    migrate_legacy_plugins_core(lua);

    assert_eq!(std::fs::read_to_string(lua.join("core").join(name)).unwrap(), bundled);
    assert!(!lua.join("plugins/core").exists());
}

#[test]
fn migrate_legacy_plugins_core_customized_copy_replaces_pristine_core() {
    let dir = tempfile::tempdir().unwrap();
    let lua = dir.path();
    let (name, _) = banner_core_file();
    seed_default_lua_core(&lua.join("core"), false);
    write_legacy_core(lua, name, "-- custom banner\n");

    migrate_legacy_plugins_core(lua);

    assert_eq!(
        std::fs::read_to_string(lua.join("core").join(name)).unwrap(),
        "-- custom banner\n"
    );
    assert!(!lua.join("plugins/core").exists());
}

#[test]
fn migrate_legacy_plugins_core_conflict_sets_legacy_aside() {
    let dir = tempfile::tempdir().unwrap();
    let lua = dir.path();
    let (name, _) = banner_core_file();
    seed_default_lua_core(&lua.join("core"), false);
    std::fs::write(lua.join("core").join(name), "-- core edit\n").unwrap();
    write_legacy_core(lua, name, "-- legacy edit\n");

    migrate_legacy_plugins_core(lua);

    let legacy = lua.join("plugins/core").join(name);
    let mut backup = legacy.as_os_str().to_os_string();
    backup.push(".bundled-backup");
    assert_eq!(
        std::fs::read_to_string(lua.join("core").join(name)).unwrap(),
        "-- core edit\n"
    );
    assert_eq!(std::fs::read_to_string(backup).unwrap(), "-- legacy edit\n");
    assert!(!legacy.exists());

    // A second run leaves the backup alone.
    migrate_legacy_plugins_core(lua);
    assert_eq!(
        std::fs::read_to_string(lua.join("core").join(name)).unwrap(),
        "-- core edit\n"
    );
}

#[test]
fn installed_plugin_names_excludes_core() {
    let dir = tempfile::tempdir().unwrap();
    let plugins = dir.path().join("plugins");
    std::fs::create_dir_all(plugins.join("core")).unwrap();
    std::fs::write(plugins.join("core/init.lua"), CORE_TOOL_LUA).unwrap();
    std::fs::create_dir_all(plugins.join("alpha")).unwrap();
    std::fs::write(plugins.join("alpha/init.lua"), ALPHA_TOOL_LUA).unwrap();

    assert_eq!(
        installed_plugin_names(&plugins),
        vec!["alpha".to_string()],
        "core must never appear among installed plugin names"
    );
}

#[test]
fn plugin_package_path_enables_in_package_requires() {
    let dir = tempfile::tempdir().unwrap();
    let plugins = dir.path().join("plugins");
    std::fs::create_dir_all(plugins.join("alpha/lib")).unwrap();
    std::fs::write(
        plugins.join("alpha/lib/helper.lua"),
        "return { answer = 42 }\n",
    )
    .unwrap();
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

    let disabled: std::collections::HashSet<String> =
        ["inactive".to_string()].into_iter().collect();
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
    assert_eq!(
        registered_tool_plugins(&lua),
        vec![Some("alpha".to_string())]
    );
    let answer: i64 = lua
        .load(r#"return require("lib.helper").answer"#)
        .eval()
        .unwrap();
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

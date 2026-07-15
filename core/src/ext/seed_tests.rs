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

    // Use a bundled command that `should_refresh_seeded_lua` doesn't
    // force-refresh by name, which would defeat the "preserved" check below.
    let (first, content) = *DEFAULT_LUA_COMMANDS
        .iter()
        .find(|(name, _)| !matches!(*name, "compact.lua" | "config.lua"))
        .expect("a non-auto-refreshed bundled command");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(first), "-- user edit\n").unwrap();

    // Without force, an existing file is left untouched.
    seed_default_lua_commands(&dir, None, false);
    assert_eq!(
        std::fs::read_to_string(dir.join(first)).unwrap(),
        "-- user edit\n",
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
    assert!(should_refresh_seeded_lua(&history, "history.lua"));

    let menu = dir.join("menu.lua");
    std::fs::write(&menu, "local pane = require(\"ui.pane\")\n").unwrap();
    assert!(should_refresh_seeded_lua(&menu, "ui/menu.lua"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn task_list_seed_refreshes_when_complete_action_is_missing() {
    let dir = std::env::temp_dir().join(format!(
        "bone-task-list-seed-test-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("task_list.lua");
    std::fs::write(&path, "-- emit_turn_message_once\n").unwrap();

    assert!(should_refresh_seeded_lua(&path, "task_list.lua"));

    let _ = std::fs::remove_dir_all(&dir);
}

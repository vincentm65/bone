use super::*;

fn lua_with_api() -> Lua {
    let lua = Lua::new();
    let bone = lua.create_table().unwrap();
    // bone.on + _handlers, then bone.api.*
    super::super::ops_events::setup_on(&lua, &bone).unwrap();
    let settings = Arc::new(Mutex::new(crate::config::settings::Settings::defaults()));
    let path = std::env::temp_dir().join("test-settings.yaml");
    let registry = Arc::new(std::sync::RwLock::new(Default::default()));
    setup_api(&lua, &bone, settings, registry, path).unwrap();
    lua.globals().set("bone", bone).unwrap();
    lua
}

#[test]
fn autocmd_for_custom_event_fires_on_emit() {
    let lua = lua_with_api();
    lua.load(
        r#"
            _G.count = 0
            bone.api.autocmd("my_event", function(payload, ctx)
                _G.count = _G.count + (payload.n or 0)
            end)
            bone.api.autocmd("my_event", function(payload, ctx)
                _G.count = _G.count + 100
            end)
            bone.api.emit("my_event", { n = 5 })
            -- Emitting an event with no handlers is a no-op.
            bone.api.emit("nobody_listening")
        "#,
    )
    .exec()
    .unwrap();
    let count: i64 = lua.globals().get("count").unwrap();
    assert_eq!(count, 105, "both handlers ran with the payload");
}

#[test]
fn top_level_keymap_accepts_strings_and_callbacks() {
    let lua = Lua::new();
    let bone = lua.create_table().unwrap();
    super::super::ops_events::setup_on(&lua, &bone).unwrap();
    let settings = Arc::new(Mutex::new(crate::config::settings::Settings::defaults()));
    setup_api(
        &lua,
        &bone,
        Arc::clone(&settings),
        Arc::new(std::sync::RwLock::new(Default::default())),
        std::env::temp_dir().join("unused-keymap-settings.yaml"),
    )
    .unwrap();
    lua.globals().set("bone", bone).unwrap();
    lua.load(
        r#"
            assert(bone.api.keymap == nil)
            bone.keymap.set("<C-p>", "toggle_panes")
            bone.keymap.set("<C-h>", function() return "/help" end)
            bone.keymap.set("<C-n>", function() end)
        "#,
    )
    .exec()
    .unwrap();

    let (callback, noop_callback) = {
        let store = settings.lock().unwrap();
        assert_eq!(store.resolved().keymaps.bindings[0].action, "toggle_panes");
        assert!(
            store.resolved().keymaps.bindings[1]
                .action
                .starts_with("__cb_")
        );
        assert!(
            store.resolved().keymaps.bindings[2]
                .action
                .starts_with("__cb_")
        );
        (
            store.resolved().keymaps.bindings[1].action.clone(),
            store.resolved().keymaps.bindings[2].action.clone(),
        )
    };
    let manager = crate::ext::types::ExtensionManager::from_arc(
        Arc::new(Mutex::new(lua)),
        true,
        true,
        Vec::new(),
        settings,
        Arc::new(std::sync::RwLock::new(Default::default())),
        crate::ext::api_ui::new_shared(),
    );
    assert!(matches!(
        manager.dispatch_keymap("toggle_panes"),
        bone_protocol::KeymapDispatchKind::Builtin { .. }
    ));
    assert!(matches!(
        manager.dispatch_keymap("summarize this"),
        bone_protocol::KeymapDispatchKind::Prompt { .. }
    ));
    assert!(matches!(
        manager.dispatch_keymap(&callback),
        bone_protocol::KeymapDispatchKind::Command { ref text } if text == "/help"
    ));
    assert!(matches!(
        manager.dispatch_keymap(&noop_callback),
        bone_protocol::KeymapDispatchKind::Noop
    ));
}

#[test]
fn canonical_namespaces_have_no_legacy_config_or_keymap_aliases() {
    let lua = lua_with_api();
    lua.load(
        r#"
            assert(type(bone.submit) == "function")
            assert(type(bone.keymap.set) == "function")
            assert(type(bone.settings.get) == "function")
            assert(type(bone.theme.load) == "function")
            assert(bone.config == nil)
            assert(bone.api.submit == nil)
            assert(bone.api.config == nil)
            assert(bone.api.keymap == nil)
        "#,
    )
    .exec()
    .unwrap();
}

#[test]
fn theme_list_load_and_reload_selected_theme() {
    let root = std::env::temp_dir().join(format!(
        "bone-lua-theme-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let themes = root.join("lua/themes");
    std::fs::create_dir_all(&themes).unwrap();
    let theme_path = themes.join("ocean.lua");
    std::fs::write(
        &theme_path,
        r##"return { palette = { accent = "#112233" }, thinking = "accent" }"##,
    )
    .unwrap();
    let settings_path = root.join("config.yaml");
    let settings = Arc::new(Mutex::new(crate::config::settings::Settings::defaults()));

    let lua = Lua::new();
    let bone = lua.create_table().unwrap();
    super::super::ops_events::setup_on(&lua, &bone).unwrap();
    setup_api(
        &lua,
        &bone,
        Arc::clone(&settings),
        Arc::new(std::sync::RwLock::new(Default::default())),
        settings_path.clone(),
    )
    .unwrap();
    lua.globals().set("bone", bone).unwrap();
    lua.load(
        r#"
            local themes = bone.theme.list()
            assert(#themes == 1 and themes[1] == "ocean")
            bone.theme.load("ocean")
        "#,
    )
    .exec()
    .unwrap();
    {
        let store = settings.lock().unwrap();
        assert_eq!(store.resolved().theme.name.as_deref(), Some("ocean"));
        assert_eq!(
            store.resolved().theme.palette.accent.as_deref(),
            Some("#112233")
        );
    }
    assert!(
        std::fs::read_to_string(&settings_path)
            .unwrap()
            .contains("name: ocean")
    );

    std::fs::write(
        &theme_path,
        r##"return { palette = { accent = "#445566" } }"##,
    )
    .unwrap();
    let lua = Lua::new();
    let bone = lua.create_table().unwrap();
    super::super::ops_events::setup_on(&lua, &bone).unwrap();
    setup_api(
        &lua,
        &bone,
        Arc::clone(&settings),
        Arc::new(std::sync::RwLock::new(Default::default())),
        settings_path,
    )
    .unwrap();
    assert_eq!(
        settings
            .lock()
            .unwrap()
            .resolved()
            .theme
            .palette
            .accent
            .as_deref(),
        Some("#445566")
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn settings_get_set_reset_persist_and_validate() {
    let lua = Lua::new();
    let bone = lua.create_table().unwrap();
    super::super::ops_events::setup_on(&lua, &bone).unwrap();
    let settings = Arc::new(Mutex::new(crate::config::settings::Settings::defaults()));
    let path = std::env::temp_dir().join(format!(
        "bone-lua-settings-{}-{}.yaml",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    setup_api(
        &lua,
        &bone,
        Arc::clone(&settings),
        Arc::new(std::sync::RwLock::new(Default::default())),
        path.clone(),
    )
    .unwrap();
    lua.globals().set("bone", bone).unwrap();

    lua.load(
        r#"
            assert(bone.settings.get("general.approval") == "safe")
            bone.settings.set("general.approval", "danger")
            assert(bone.settings.get("general.approval") == "danger")
            assert(not pcall(bone.settings.set, "general.approval", "invalid"))
            assert(bone.settings.get("general.approval") == "danger")
            assert(bone.settings.reset("general.approval") == "safe")
        "#,
    )
    .exec()
    .unwrap();

    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(raw.contains("approval: safe"));
    assert_eq!(settings.lock().unwrap().resolved().general.approval, "safe");
    let _ = std::fs::remove_file(path);
}

#[test]
fn extension_settings_register_resolve_validate_and_persist() {
    let lua = Lua::new();
    let bone = lua.create_table().unwrap();
    super::super::ops_events::setup_on(&lua, &bone).unwrap();
    let settings = Arc::new(Mutex::new(crate::config::settings::Settings::defaults()));
    let registry = Arc::new(std::sync::RwLock::new(Default::default()));
    let path = std::env::temp_dir().join(format!(
        "bone-extension-settings-{}-{}.yaml",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    setup_api(
        &lua,
        &bone,
        Arc::clone(&settings),
        Arc::clone(&registry),
        path.clone(),
    )
    .unwrap();
    lua.globals().set("bone", bone).unwrap();
    lua.load(
        r#"
        bone.settings.register({
          namespace = "example",
          title = "Example",
          fields = {
            { key = "enabled", label = "Enabled", type = "bool", default = true },
            { key = "limit", label = "Limit", type = "number", default = 10,
              integer = true, min = 1, max = 100 },
            { key = "mode", label = "Mode", type = "enum", default = "fast",
              options = { "fast", "safe" } },
          },
        })
        assert(bone.settings._get_extension("example.enabled") == true)
        assert(not pcall(bone.settings._set_extension, "example.limit", 1.5))
        bone.settings._set_extension("example.limit", 25)
        assert(bone.settings._get_extension("example.limit") == 25)
        assert(not pcall(bone.settings.register, {
          namespace = "example", title = "Duplicate",
          fields = {{ key = "x", label = "X", type = "bool", default = true }},
        }))
        "#,
    )
    .exec()
    .unwrap();

    assert_eq!(registry.read().unwrap().pages().len(), 1);
    assert_eq!(
        settings.lock().unwrap().extension_value("example.limit"),
        Some(&crate::config::settings::ExtensionValue::Number(25.0))
    );
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(raw.contains("extensions:"));
    assert!(raw.contains("limit: 25"));
    let _ = std::fs::remove_file(path);
}

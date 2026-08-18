use super::*;

fn managed_hook_manager(script: &str) -> ExtensionManager {
    let lua = Lua::new();
    let bone = lua.create_table().unwrap();
    let settings = Arc::new(Mutex::new(crate::config::settings::Settings::defaults()));
    let registry = Arc::new(std::sync::RwLock::new(Default::default()));
    let ui = crate::ext::api_ui::new_shared();
    super::super::ops_events::setup_on(&lua, &bone).unwrap();
    lua.globals().set("bone", bone.clone()).unwrap();
    super::super::api::setup_api(
        &lua,
        &bone,
        Arc::clone(&settings),
        Arc::clone(&registry),
        std::env::temp_dir().join("bone-managed-hook-settings.yaml"),
        Arc::clone(&ui),
    )
    .unwrap();
    lua.load(script).exec().unwrap();
    ExtensionManager::from_arc(
        Arc::new(Mutex::new(lua)),
        true,
        true,
        Vec::new(),
        settings,
        registry,
        ui,
    )
}

fn managed_hook_ctx() -> crate::ext::ctx::CtxConfig {
    let store = crate::config::store::ConfigStore::for_test();
    let mut cfg = crate::ext::ctx::CtxConfig::new(
        std::env::temp_dir().to_string_lossy().to_string(),
        Arc::new(Mutex::new(Default::default())),
    );
    cfg.config_schema = Some(store.schema());
    cfg.config_store = Some(store);
    cfg
}

#[test]
fn unloaded_manager_exposes_inert_defaults() {
    let manager = ExtensionManager::unloaded();

    assert!(!manager.is_available());
    assert!(manager.commands().is_empty());
    assert!(matches!(
        manager.dispatch_tool_call("create_file", "call_1", &serde_json::json!({}), "danger"),
        EventDispatchResult::Continue
    ));

    let settings = manager.frontend_settings().settings;
    assert_eq!(settings.general.approval, "safe");
    assert!(settings.ui.status_show_model);
    assert!(settings.theme.highlights.is_empty());
    assert!(settings.keymaps.bindings.is_empty());
}

#[test]
fn rebinding_runtime_inbox_preserves_queued_prompts() {
    let durable = super::super::inbox::SubmitInbox::default();
    durable.push("before reload".into());

    let mut reloaded = ExtensionManager::unloaded();
    reloaded.submit_inbox().push("during reload".into());
    reloaded.use_submit_inbox(durable.clone());

    assert_eq!(durable.drain(), vec!["before reload", "during reload"]);

    let lua = reloaded.lua_handle();
    super::super::inbox::for_lua(&lua.lock().unwrap()).push("after reload".into());
    assert_eq!(
        reloaded.submit_inbox().pop().as_deref(),
        Some("after reload")
    );
}

#[test]
fn dropping_clone_does_not_stop_runtime_but_root_drop_does() {
    let root = ExtensionManager::unloaded();
    let flag = root.runtime_stopped();

    let clone = root.clone();
    drop(clone);
    assert!(
        !flag.load(Ordering::Acquire),
        "dropping a per-turn/per-hook clone must not cancel pending timers"
    );

    // A clone-of-a-clone is equally inert.
    drop(root.clone().clone());
    assert!(!flag.load(Ordering::Acquire));

    drop(root);
    assert!(
        flag.load(Ordering::Acquire),
        "dropping the root manager must signal runtime teardown"
    );
}

#[test]
fn extension_settings_pages_follow_command_enablement() {
    use super::super::settings_registry::{SettingsField, SettingsFieldType, SettingsPage};
    use crate::config::settings::ExtensionValue;

    let manager = ExtensionManager::unloaded();
    let page = |namespace: &str, command: Option<&str>| SettingsPage {
        namespace: namespace.into(),
        title: namespace.into(),
        owner: "test.lua".into(),
        command: command.map(str::to_string),
        fields: vec![SettingsField {
            key: "value".into(),
            label: "Value".into(),
            field_type: SettingsFieldType::String,
            options: Vec::new(),
            default: ExtensionValue::String("default".into()),
            value: None,
            integer: None,
            min: None,
            max: None,
        }],
    };
    let registry = manager.settings_registry.clone();
    let mut registry = registry.write().unwrap();
    registry.register(page("example", None)).unwrap();
    registry.register(page("renamed", Some("example"))).unwrap();
    registry.register(page("standalone", None)).unwrap();
    drop(registry);

    let mut settings = crate::config::settings::Settings::defaults();
    settings.inner.commands.disabled.push("example".into());
    let extensions = std::collections::BTreeMap::from([(
        "example".into(),
        std::collections::BTreeMap::from([(
            "value".into(),
            ExtensionValue::String("persisted".into()),
        )]),
    )]);
    settings.replace_domains(std::collections::BTreeMap::new(), extensions);
    manager.replace_settings(settings.clone());

    let namespaces = |pages: &[SettingsPage]| {
        pages
            .iter()
            .map(|page| page.namespace.clone())
            .collect::<Vec<_>>()
    };
    let pages = manager.extension_settings_pages();
    assert_eq!(namespaces(&pages), ["standalone"]);

    settings.inner.commands.disabled.clear();
    manager.replace_settings(settings);
    let pages = manager.extension_settings_pages();
    assert_eq!(namespaces(&pages), ["example", "renamed", "standalone"]);
    assert_eq!(
        pages[0].fields[0].value,
        Some(ExtensionValue::String("persisted".into()))
    );
}

fn msg_table(lua: &Lua, role: &str, content: &str) -> mlua::Table {
    let t = lua.create_table().unwrap();
    t.set("role", role).unwrap();
    t.set("content", content).unwrap();
    t
}

#[test]
fn parses_conversation_load_with_id() {
    let lua = Lua::new();
    let messages = lua.create_table().unwrap();
    messages.push(msg_table(&lua, "user", "hi")).unwrap();
    messages
        .push(msg_table(&lua, "assistant", "hello"))
        .unwrap();
    let action = lua.create_table().unwrap();
    action.set("action", "conversation.load").unwrap();
    action.set("messages", messages).unwrap();
    action.set("conversation_id", 7i64).unwrap();

    let parsed = parse_lua_return_action(&action, false).expect("action parsed");
    assert!(parsed.conversation_replace.is_none());
    let load = parsed.conversation_load.expect("load payload");
    assert_eq!(load.conversation_id, Some(7));
    assert_eq!(load.messages.len(), 2);
    assert_eq!(load.messages[0].content, "hi");
}

#[test]
fn conversation_replace_still_parses() {
    let lua = Lua::new();
    let messages = lua.create_table().unwrap();
    messages.push(msg_table(&lua, "user", "hi")).unwrap();
    let action = lua.create_table().unwrap();
    action.set("action", "conversation.replace").unwrap();
    action.set("messages", messages).unwrap();

    let parsed = parse_lua_return_action(&action, false).expect("action parsed");
    assert!(parsed.conversation_load.is_none());
    assert_eq!(parsed.conversation_replace.expect("replace").len(), 1);
}

#[test]
fn turn_shaping_fields_parse_before_turn_and_on_command_returns() {
    let lua = Lua::new();
    let action = lua.create_table().unwrap();
    action.set("system_prompt_append", "Plan only.").unwrap();
    action.set("turn_message", "Current state.").unwrap();
    let tools = lua.create_table().unwrap();
    tools.push("read_file").unwrap();
    action.set("tool_filter", tools).unwrap();

    for before_turn in [true, false] {
        let parsed = parse_lua_return_action(&action, before_turn).unwrap();
        assert_eq!(parsed.system_prompt_append.as_deref(), Some("Plan only."));
        assert_eq!(parsed.turn_message.as_deref(), Some("Current state."));
        assert_eq!(parsed.tool_filter, Some(vec!["read_file".to_string()]));
        assert!(parsed.conversation_replace.is_none());
    }
}

#[test]
fn empty_table_yields_no_action() {
    let lua = Lua::new();
    let t = lua.create_table().unwrap();
    assert!(parse_lua_return_action(&t, false).is_none());
}

#[test]
fn command_action_round_trips_through_wire_type() {
    let action = LuaReturnAction {
        conversation_load: Some(ConversationLoad {
            messages: vec![crate::llm::ChatMessage::new(
                crate::llm::ChatRole::User,
                "past",
            )],
            conversation_id: Some(9),
        }),
        config_action: Some(ConfigAction::SwitchProvider {
            id: "anthropic".into(),
        }),
        // before_turn-only fields must be dropped on the way to the wire.
        system_prompt_append: Some("ignored".into()),
        tool_filter: Some(vec!["read_file".into()]),
        ..Default::default()
    };

    let wire = action
        .to_command_action()
        .expect("command-relevant fields set");
    let back: LuaReturnAction = wire.into();

    let load = back.conversation_load.expect("load survived");
    assert_eq!(load.conversation_id, Some(9));
    assert_eq!(load.messages.len(), 1);
    assert!(matches!(
        back.config_action,
        Some(ConfigAction::SwitchProvider { id }) if id == "anthropic"
    ));
    // Turn-shaping fields don't cross the command path.
    assert!(back.system_prompt_append.is_none());
    assert!(back.tool_filter.is_none());
}

#[test]
fn turn_shaping_only_action_has_no_command_action() {
    let action = LuaReturnAction {
        system_prompt_append: Some("Plan only.".into()),
        ..Default::default()
    };
    assert!(action.to_command_action().is_none());
}

#[test]
fn parses_conversation_load_with_only_id() {
    let lua = Lua::new();
    let action = lua.create_table().unwrap();
    action.set("action", "conversation.load").unwrap();
    action.set("conversation_id", 7i64).unwrap();

    let parsed = parse_lua_return_action(&action, false).expect("action parsed");
    let load = parsed.conversation_load.expect("load payload");
    assert_eq!(load.conversation_id, Some(7));
    assert!(load.messages.is_empty());
}

#[test]
fn conversation_load_without_id_is_ignored() {
    let lua = Lua::new();
    let messages = lua.create_table().unwrap();
    messages.push(msg_table(&lua, "user", "hi")).unwrap();
    let action = lua.create_table().unwrap();
    action.set("action", "conversation.load").unwrap();
    action.set("messages", messages).unwrap();

    assert!(parse_lua_return_action(&action, false).is_none());
}

#[test]
fn managed_hooks_run_in_order_with_full_context_and_collect_operations() {
    let manager = managed_hook_manager(
        r#"
        _G.seen = {}
        bone.on("custom", function(event, ctx)
            table.insert(_G.seen, "first:" .. event.value)
            ctx.state.set("hook", "available")
            ctx.conversation.append({ { role = "assistant", content = "from hook" } })
        end)
        bone.on("custom", function(_, ctx)
            table.insert(_G.seen, "second:" .. ctx.state.get("hook"))
        end)
        "#,
    );

    let result = manager.dispatch_managed(
        "custom",
        serde_json::json!({ "value": "payload" }),
        managed_hook_ctx(),
        false,
    );
    assert!(result.blocked.is_none());
    assert_eq!(result.operations.len(), 1);
    let crate::ext::ctx::ConversationOperation::Append(messages) = &result.operations[0] else {
        panic!("expected append operation");
    };
    assert_eq!(messages[0].content, "from hook");

    let lua = manager.lua_handle();
    let lua = lua.lock().unwrap();
    let seen: mlua::Table = lua.globals().get("seen").unwrap();
    assert_eq!(seen.get::<String>(1).unwrap(), "first:payload");
    assert_eq!(seen.get::<String>(2).unwrap(), "second:available");
}

#[test]
fn managed_hook_timeout_fails_open_and_later_handlers_run() {
    let manager = managed_hook_manager(
        r#"
        _G.after_timeout = false
        bone.on("tool_call", function()
            while true do end
        end, { timeout_ms = 100 })
        bone.on("tool_call", function()
            _G.after_timeout = true
            return { block = true, reason = "blocked second" }
        end)
        "#,
    );

    let started = std::time::Instant::now();
    let result =
        manager.dispatch_managed("tool_call", serde_json::json!({}), managed_hook_ctx(), true);
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    assert_eq!(result.blocked.as_deref(), Some("blocked second"));
    let lua = manager.lua_handle();
    assert!(
        lua.lock()
            .unwrap()
            .globals()
            .get::<bool>("after_timeout")
            .unwrap()
    );
}

#[test]
fn managed_hook_cancellation_interrupts_lua_and_cleans_up_hook() {
    let manager = managed_hook_manager(
        r#"
        bone.on("custom", function()
            while true do end
        end, { timeout_ms = 60000 })
        "#,
    );
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let mut cfg = managed_hook_ctx();
    cfg.cancelled = Some(cancelled);

    let started = std::time::Instant::now();
    let result = manager.dispatch_managed("custom", serde_json::json!({}), cfg, false);
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    assert!(result.operations.is_empty());

    // A completed dispatch must remove the VM hook; unrelated Lua remains usable.
    let lua = manager.lua_handle();
    assert_eq!(
        lua.lock()
            .unwrap()
            .load("return 2 + 2")
            .eval::<i64>()
            .unwrap(),
        4
    );
}

#[test]
fn managed_hook_suppresses_lifecycle_emit_but_allows_custom_emit() {
    let manager = managed_hook_manager(
        r#"
        _G.lifecycle_calls = 0
        _G.custom_calls = 0
        bone.on("turn_start", function()
            _G.lifecycle_calls = _G.lifecycle_calls + 1
            bone.api.emit("turn_start")
            bone.api.emit("custom")
        end)
        bone.on("custom", function()
            _G.custom_calls = _G.custom_calls + 1
        end)
        "#,
    );

    manager.dispatch_managed(
        "turn_start",
        serde_json::json!({}),
        managed_hook_ctx(),
        false,
    );

    let lua = manager.lua_handle();
    let lua = lua.lock().unwrap();
    assert_eq!(lua.globals().get::<i64>("lifecycle_calls").unwrap(), 1);
    assert_eq!(lua.globals().get::<i64>("custom_calls").unwrap(), 1);
}

#[test]
fn nested_managed_dispatch_is_suppressed() {
    let manager = managed_hook_manager(
        r#"
        _G.calls = 0
        bone.on("turn_start", function()
            _G.calls = _G.calls + 1
        end)
        "#,
    );
    let lua = manager.lua_handle();
    let nested = manager.clone();
    {
        let lua = lua.lock().unwrap();
        let callback = lua
            .create_function(move |_, ()| {
                nested.dispatch_managed(
                    "turn_start",
                    serde_json::json!({}),
                    managed_hook_ctx(),
                    false,
                );
                Ok(())
            })
            .unwrap();
        lua.globals().set("nested_dispatch", callback).unwrap();
    }
    // Replace the registered callback with one that attempts synchronous re-entry.
    lua.lock()
        .unwrap()
        .load(
            r#"
            bone._handlers.turn_start[1] = function()
                _G.calls = _G.calls + 1
                nested_dispatch()
            end
            "#,
        )
        .exec()
        .unwrap();

    manager.dispatch_managed(
        "turn_start",
        serde_json::json!({}),
        managed_hook_ctx(),
        false,
    );
    assert_eq!(
        lua.lock().unwrap().globals().get::<i64>("calls").unwrap(),
        1
    );
}

#[test]
fn parses_config_actions() {
    let lua = Lua::new();

    let apply = lua.create_table().unwrap();
    apply.set("action", "config.apply").unwrap();
    let parsed = parse_lua_return_action(&apply, false).expect("apply action");
    assert!(matches!(parsed.config_action, Some(ConfigAction::Apply)));

    let reload = lua.create_table().unwrap();
    reload.set("action", "config.reload_tools").unwrap();
    let parsed = parse_lua_return_action(&reload, false).expect("reload action");
    assert!(matches!(
        parsed.config_action,
        Some(ConfigAction::ReloadTools)
    ));

    let switch = lua.create_table().unwrap();
    switch.set("action", "config.switch_provider").unwrap();
    switch.set("provider", "openai").unwrap();
    let parsed = parse_lua_return_action(&switch, false).expect("switch action");
    assert!(matches!(
        parsed.config_action,
        Some(ConfigAction::SwitchProvider { ref id }) if id == "openai"
    ));
}

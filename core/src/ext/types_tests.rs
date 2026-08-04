use super::*;

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

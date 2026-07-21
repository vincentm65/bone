use super::*;

#[test]
fn unloaded_manager_has_a_compatibility_snapshot() {
    let manager = ExtensionManager::unloaded();
    assert!(manager.config_snapshot().pages.is_empty());
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

    let parsed = parse_lua_return_action(&action).expect("action parsed");
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

    let parsed = parse_lua_return_action(&action).expect("action parsed");
    assert!(parsed.conversation_load.is_none());
    assert_eq!(parsed.conversation_replace.expect("replace").len(), 1);
}

#[test]
fn parses_turn_shaping_without_action() {
    let lua = Lua::new();
    let t = lua.create_table().unwrap();
    t.set("system_prompt_append", "Plan only; do not edit.")
        .unwrap();
    t.set("turn_message", "Task list: 1/3 done.").unwrap();
    let tools = lua.create_table().unwrap();
    tools.push("read_file").unwrap();
    tools.push("shell").unwrap();
    t.set("tool_filter", tools).unwrap();

    let parsed = parse_lua_return_action(&t).expect("shaping parsed without action key");
    assert_eq!(
        parsed.system_prompt_append.as_deref(),
        Some("Plan only; do not edit.")
    );
    assert_eq!(parsed.turn_message.as_deref(), Some("Task list: 1/3 done."));
    assert_eq!(
        parsed.tool_filter,
        Some(vec!["read_file".to_string(), "shell".to_string()])
    );
    assert!(parsed.conversation_replace.is_none());
}

#[test]
fn empty_table_yields_no_action() {
    let lua = Lua::new();
    let t = lua.create_table().unwrap();
    assert!(parse_lua_return_action(&t).is_none());
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

    let parsed = parse_lua_return_action(&action).expect("action parsed");
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

    assert!(parse_lua_return_action(&action).is_none());
}

#[test]
fn parses_config_actions() {
    let lua = Lua::new();

    let apply = lua.create_table().unwrap();
    apply.set("action", "config.apply").unwrap();
    let parsed = parse_lua_return_action(&apply).expect("apply action");
    assert!(matches!(parsed.config_action, Some(ConfigAction::Apply)));

    let reload = lua.create_table().unwrap();
    reload.set("action", "config.reload_tools").unwrap();
    let parsed = parse_lua_return_action(&reload).expect("reload action");
    assert!(matches!(
        parsed.config_action,
        Some(ConfigAction::ReloadTools)
    ));

    let switch = lua.create_table().unwrap();
    switch.set("action", "config.switch_provider").unwrap();
    switch.set("provider", "openai").unwrap();
    let parsed = parse_lua_return_action(&switch).expect("switch action");
    assert!(matches!(
        parsed.config_action,
        Some(ConfigAction::SwitchProvider { ref id }) if id == "openai"
    ));
}

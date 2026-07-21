use super::*;
use mlua::Lua;

#[test]
fn get_bone_absent_returns_none() {
    let lua = Lua::new();
    assert!(get_bone(&lua).is_none());
}

#[test]
fn get_bone_present_returns_table() {
    let lua = Lua::new();
    let bone = lua.create_table().unwrap();
    lua.globals().set("bone", bone).unwrap();
    assert!(get_bone(&lua).is_some());
}

#[test]
fn boot_warnings_are_routed_to_lua_log() {
    let dir = std::env::temp_dir().join(format!(
        "bone-loader-warning-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();

    log_boot_warning(&dir, "Lua tools failed: test error");

    let log = std::fs::read_to_string(dir.join("bone.log")).unwrap();
    assert!(log.contains("bone-lua [warn]: Lua tools failed: test error"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn boot_loads_config_subagents_before_lua_with_config_precedence() {
    use crate::config::settings::SubagentSettings;

    let dir = std::env::temp_dir().join(format!(
        "bone-loader-subagents-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(dir.join("lua/commands")).unwrap();
    std::fs::write(
        dir.join("lua/commands/agents.lua"),
        r#"bone.command.register("agents", {
  description = "manage named sub-agents",
  handler = function() end,
})"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("init.lua"),
        r#"
bone.subagent.register({ name = "shared", description = "from lua" })
bone.subagent.register({ name = "disabled", description = "must stay disabled" })
bone.subagent.register({ name = "lua-only", description = "lua agent" })
"#,
    )
    .unwrap();

    let mut settings = Settings::defaults();
    settings.inner.subagents.insert(
        "shared".into(),
        SubagentSettings {
            description: "from config".into(),
            system_prompt: Some("configured prompt".into()),
            max_concurrency: Some(6),
            ..Default::default()
        },
    );
    settings.inner.subagents.insert(
        "disabled".into(),
        SubagentSettings {
            description: "disabled agent".into(),
            enabled: false,
            ..Default::default()
        },
    );

    let result = boot(
        &dir,
        &dir,
        BootOptions::default(),
        "test-model",
        "test-provider",
        Some(Arc::new(Mutex::new(settings))),
    );

    let lua = result.manager.lua_arc();
    let lua = lua.lock().unwrap();
    let entries: mlua::Table = lua
        .globals()
        .get::<mlua::Table>("bone")
        .unwrap()
        .get("_subagents")
        .unwrap();
    let agents: Vec<(String, String, Option<usize>)> = entries
        .sequence_values::<mlua::Table>()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.get("name").unwrap(),
                entry.get("description").unwrap(),
                entry.get("max_concurrency").ok(),
            )
        })
        .collect();
    drop(entries);
    drop(lua);

    assert_eq!(
        agents,
        vec![
            ("shared".into(), "from config".into(), Some(6)),
            ("lua-only".into(), "lua agent".into(), None),
        ]
    );
    assert!(
        result
            .manager
            .commands()
            .iter()
            .any(|command| command.name == "agents")
    );
    let advertised = result.manager.subagents();
    assert_eq!(advertised.len(), 3);
    assert!(advertised.iter().any(|agent| {
        agent.name == "shared" && agent.description == "from config" && agent.source == "config"
    }));
    assert!(
        advertised
            .iter()
            .any(|agent| agent.name == "lua-only" && agent.source == "lua")
    );
    assert!(
        advertised
            .iter()
            .any(|agent| agent.name == "disabled" && !agent.enabled)
    );

    std::fs::remove_dir_all(dir).unwrap();
}

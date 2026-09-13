use super::*;

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
fn boot_reports_invalid_init_source() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("init.lua"), "this is not valid lua (").unwrap();

    let result = boot(
        dir.path(),
        dir.path(),
        BootOptions {
            agent_depth: 1,
            ..Default::default()
        },
        "test-model",
        "test-provider",
        Some(Arc::new(Mutex::new(Settings::defaults()))),
    );

    assert!(result.manager.is_available());
    assert!(
        result
            .source_errors
            .iter()
            .any(|error| error.contains("init.lua error"))
    );
}

#[test]
fn boot_reports_invalid_tool_source() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("init.lua"), "-- valid").unwrap();
    std::fs::create_dir_all(dir.path().join("lua/tools")).unwrap();
    std::fs::write(
        dir.path().join("lua/tools/broken.lua"),
        "this is not valid lua (",
    )
    .unwrap();

    let result = boot(
        dir.path(),
        dir.path(),
        BootOptions {
            agent_depth: 1,
            ..Default::default()
        },
        "test-model",
        "test-provider",
        Some(Arc::new(Mutex::new(Settings::defaults()))),
    );

    assert!(result.manager.is_available());
    assert!(
        result
            .source_errors
            .iter()
            .any(|error| error.contains("broken.lua"))
    );
}

#[test]
fn boot_runs_nested_lua_init() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("lua")).unwrap();
    std::fs::write(dir.path().join("lua/init.lua"), "nested_init_ran = true").unwrap();

    let result = boot(
        dir.path(),
        dir.path(),
        BootOptions {
            agent_depth: 1,
            ..Default::default()
        },
        "test-model",
        "test-provider",
        Some(Arc::new(Mutex::new(Settings::defaults()))),
    );

    assert!(result.manager.is_available());
    assert!(
        result.source_errors.is_empty(),
        "unexpected errors: {:?}",
        result.source_errors
    );
    // A nested init satisfies startup, so no blank root init.lua is created.
    assert!(!dir.path().join("init.lua").exists());
    let lua = result.manager.lua_arc();
    let lua = lua.lock().unwrap();
    assert!(lua.globals().get::<bool>("nested_init_ran").unwrap());
}

#[test]
fn boot_ignores_uppercase_lua_tool_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("init.lua"), "-- valid").unwrap();
    std::fs::create_dir_all(dir.path().join("lua/tools")).unwrap();
    std::fs::write(
        dir.path().join("lua/tools/Foo.lua"),
        "this is not valid lua (",
    )
    .unwrap();

    let result = boot(
        dir.path(),
        dir.path(),
        BootOptions {
            agent_depth: 1,
            ..Default::default()
        },
        "test-model",
        "test-provider",
        Some(Arc::new(Mutex::new(Settings::defaults()))),
    );

    assert!(result.manager.is_available());
    assert!(
        result
            .source_errors
            .iter()
            .all(|error| !error.contains("Foo.lua")),
        "uppercase file should not be loaded: {:?}",
        result.source_errors
    );
}

#[test]
fn boot_exposes_helpers_dir_in_bone_metadata() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("init.lua"), "-- valid").unwrap();

    let result = boot(
        dir.path(),
        dir.path(),
        BootOptions {
            agent_depth: 1,
            ..Default::default()
        },
        "test-model",
        "test-provider",
        Some(Arc::new(Mutex::new(Settings::defaults()))),
    );

    assert!(result.manager.is_available());
    let lua = result.manager.lua_arc();
    let lua = lua.lock().unwrap();
    let bone: mlua::Table = lua.globals().get("bone").unwrap();
    let helpers_dir: String = bone.get("helpers_dir").unwrap();
    assert!(
        std::path::Path::new(&helpers_dir).ends_with("lua/helpers"),
        "unexpected helpers_dir: {helpers_dir}"
    );
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
    let mut subagents = std::collections::BTreeMap::new();
    subagents.insert(
        "shared".into(),
        SubagentSettings {
            description: "from config".into(),
            system_prompt: Some("configured prompt".into()),
            ..Default::default()
        },
    );
    subagents.insert(
        "disabled".into(),
        SubagentSettings {
            description: "disabled agent".into(),
            enabled: false,
            ..Default::default()
        },
    );
    settings.replace_domains(subagents, std::collections::BTreeMap::new());

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
    let agents: Vec<(String, String)> = entries
        .sequence_values::<mlua::Table>()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.get("name").unwrap(),
                entry.get("description").unwrap(),
            )
        })
        .collect();
    drop(entries);
    drop(lua);

    assert_eq!(
        agents,
        vec![
            ("shared".into(), "from config".into()),
            ("lua-only".into(), "lua agent".into()),
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

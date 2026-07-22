use bone_core::ext::{self, BootOptions};

#[test]
fn lua_subagent_api_lists_daemon_snapshot_without_direct_mutations() {
    let config_dir = std::env::temp_dir().join(format!(
        "bone-lua-subagent-api-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("config.yaml"), "version: 2\n").unwrap();
    std::fs::write(
        config_dir.join("subagents.yaml"),
        r#"version: 1
subagents:
  disabled:
    description: Disabled config agent
    enabled: false
  shared:
    description: Config wins
    approval: danger
    max_concurrency: 3
"#,
    )
    .unwrap();
    std::fs::write(
        config_dir.join("init.lua"),
        r#"
bone.subagent.register({ name = "shared", description = "Lua loses" })
bone.subagent.register({ name = "lua-only", description = "Lua agent", max_concurrency = 7 })
"#,
    )
    .unwrap();
    unsafe { std::env::set_var("BONE_DIR", &config_dir) };

    let booted = ext::boot(
        &config_dir,
        &config_dir,
        BootOptions::default(),
        "model",
        "provider",
    );
    let _config = bone_core::config::store::ConfigStore::new(booted.manager.clone()).unwrap();
    let lua = booted.manager.lua_arc();
    let lua = lua.lock().unwrap();

    lua.load(
        r#"
local agents = bone.subagent.list()
assert(#agents == 3)
assert(agents[1].name == "disabled" and agents[1].source == "config" and agents[1].enabled == false)
assert(agents[2].name == "lua-only" and agents[2].source == "lua" and agents[2].max_concurrency == 7)
assert(agents[3].name == "shared" and agents[3].source == "config" and agents[3].max_concurrency == 3)
assert(agents[3].description == "Config wins")

local first = bone.subagent.list()
first[1].description = "mutated snapshot"
assert(bone.subagent.list()[1].description == "Disabled config agent")
assert(bone.subagent.upsert == nil)
assert(bone.subagent.delete == nil)
assert(bone.subagent.set_enabled == nil)
"#,
    )
    .exec()
    .unwrap();
    drop(lua);

    std::fs::remove_dir_all(config_dir).unwrap();
}

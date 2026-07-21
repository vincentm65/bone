use bone_core::config::settings::Settings;
use bone_core::ext::{self, BootOptions};

#[test]
fn lua_subagent_management_api_lists_validates_and_persists() {
    let config_dir = std::env::temp_dir().join(format!(
        "bone-lua-subagent-api-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.yaml"),
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

bone.subagent.upsert({
  name = "reviewer",
  description = "Reviews changes",
  system_prompt = "Find concrete regressions.",
  provider = "anthropic",
  model = "opus",
  approval = "danger",
  timeout_ms = 300000,
  max_concurrency = 4,
  enabled = true,
})
bone.subagent.upsert({
  name = "blank-options",
  description = "Normalizes blanks",
  system_prompt = "",
  provider = "",
  model = "",
  approval = "safe",
  enabled = true,
})
bone.subagent.set_enabled("reviewer", false)
local normalized
for _, agent in ipairs(bone.subagent.list()) do
  if agent.name == "blank-options" then normalized = agent end
end
assert(normalized and normalized.system_prompt == nil and normalized.provider == nil and normalized.model == nil)

for _, bad in ipairs({
  { name = "bad name", description = "x", approval = "safe", enabled = true },
  { name = "valid", description = "", approval = "safe", enabled = true },
  { name = "valid", description = "x", approval = "maybe", enabled = true },
  { name = "valid", description = "x", approval = "safe", timeout_ms = 0, enabled = true },
  { name = "valid", description = "x", approval = "safe", timeout_ms = 900001, enabled = true },
  { name = "valid", description = "x", approval = "safe", max_concurrency = 0, enabled = true },
}) do
  assert(not pcall(bone.subagent.upsert, bad))
end
assert(not pcall(bone.subagent.delete, "lua-only"))
bone.subagent.upsert({
  name = "lua-only",
  description = "Edited Lua agent",
  approval = "danger",
  max_concurrency = 8,
  enabled = true,
})
bone.subagent.set_enabled("lua-only", false)
local lua_override
for _, agent in ipairs(bone.subagent.list()) do
  if agent.name == "lua-only" then lua_override = agent end
end
assert(lua_override and lua_override.source == "config" and lua_override.enabled == false)
assert(lua_override.description == "Edited Lua agent" and lua_override.approval == "danger")
assert(lua_override.max_concurrency == 8)

bone.subagent.delete("blank-options")
local refreshed = bone.subagent.list()
for _, agent in ipairs(refreshed) do assert(agent.name ~= "blank-options") end
"#,
    )
    .exec()
    .unwrap();

    let blocked = config_dir.join("not-a-directory");
    std::fs::write(&blocked, "blocked").unwrap();
    unsafe { std::env::set_var("BONE_DIR", &blocked) };
    let persistence_error: String = lua
        .load(
            r#"
local ok, err = pcall(bone.subagent.upsert, {
  name = "not-persisted",
  description = "Must fail",
  approval = "safe",
  enabled = true,
})
assert(not ok)
return tostring(err)
"#,
        )
        .eval()
        .unwrap();
    assert!(!persistence_error.is_empty());
    unsafe { std::env::set_var("BONE_DIR", &config_dir) };
    lua.load(
        r#"
for _, agent in ipairs(bone.subagent.list()) do
  assert(agent.name ~= "not-persisted")
end
"#,
    )
    .exec()
    .unwrap();
    drop(lua);

    let saved = Settings::load().unwrap().unwrap();
    let reviewer = &saved.resolved().subagents["reviewer"];
    assert_eq!(reviewer.description, "Reviews changes");
    assert_eq!(
        reviewer.system_prompt.as_deref(),
        Some("Find concrete regressions.")
    );
    assert_eq!(reviewer.provider.as_deref(), Some("anthropic"));
    assert_eq!(reviewer.model.as_deref(), Some("opus"));
    assert_eq!(reviewer.approval, "danger");
    assert_eq!(reviewer.timeout_ms, Some(300_000));
    assert_eq!(reviewer.max_concurrency, Some(4));
    assert!(!reviewer.enabled);
    let lua_override = &saved.resolved().subagents["lua-only"];
    assert_eq!(lua_override.description, "Edited Lua agent");
    assert_eq!(lua_override.approval, "danger");
    assert_eq!(lua_override.max_concurrency, Some(8));
    assert!(!lua_override.enabled);
    assert!(!saved.resolved().subagents.contains_key("blank-options"));

    std::fs::remove_dir_all(config_dir).unwrap();
}

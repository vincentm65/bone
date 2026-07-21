//! `bone.tool.register(table)` and `bone.subagent.register(table)` bindings.

use std::sync::{Arc, Mutex};

use mlua::{Lua, LuaSerdeExt, Table, Value};

use crate::config::settings::{BoneSettings, Settings};

/// Create `bone.tool.register` and the `bone._tools` storage array.
pub(crate) fn setup_register_tool(lua: &Lua, bone: &Table) -> Result<(), String> {
    let tools_array = lua.create_table().map_err(crate::util::errstr)?;
    bone.set("_tools", tools_array)
        .map_err(crate::util::errstr)?;

    let register_fn = lua
        .create_function(|lua, args: Table| {
            // Lightweight check: name must be present so warnings are attributable.
            // Full validation of description, parameters, safety, and execute
            // happens in LuaTool::from_entry during tool collection.
            if args.get::<String>("name").is_err() {
                crate::ext::ctx::runtime_warn_once(
                    "bone-lua warn: register_tool: missing or invalid 'name'; skipping",
                );
                return Ok(());
            }

            // Store the entry as-is; from_entry validates the rest.
            let tools: Table = lua.globals().get::<Table>("bone")?.get("_tools")?;
            tools.push(args)?;
            Ok(())
        })
        .map_err(crate::util::errstr)?;

    let tool = lua.create_table().map_err(crate::util::errstr)?;
    tool.set("register", register_fn)
        .map_err(crate::util::errstr)?;
    bone.set("tool", tool).map_err(crate::util::errstr)?;
    Ok(())
}

/// Create `bone.subagent.register` and the `bone._subagents` storage table.
pub(crate) fn setup_register_subagent(
    lua: &Lua,
    bone: &Table,
    settings: Arc<Mutex<Settings>>,
) -> Result<(), String> {
    let subagents_table = lua.create_table().map_err(crate::util::errstr)?;
    bone.set("_subagents", subagents_table)
        .map_err(crate::util::errstr)?;
    bone.set(
        "_config_subagent_names",
        lua.create_table().map_err(crate::util::errstr)?,
    )
    .map_err(crate::util::errstr)?;

    let register_fn = lua
        .create_function(|lua, args: Table| {
            let name: String = match args.get("name") {
                Ok(n) => n,
                Err(_) => {
                    crate::ext::ctx::runtime_warn_once(
                        "bone-lua warn: register_subagent: missing or invalid 'name'; skipping",
                    );
                    return Ok(());
                }
            };
            if name.is_empty() {
                crate::ext::ctx::runtime_warn_once(
                    "bone-lua warn: register_subagent: empty 'name'; skipping",
                );
                return Ok(());
            }

            // Config-backed definitions intentionally override matching Lua
            // registrations. This is expected after /agents promotes a Lua
            // definition, so skip it without reporting a duplicate warning.
            let configured: Table = lua
                .globals()
                .get::<Table>("bone")?
                .get("_config_subagent_names")?;
            if configured.contains_key(name.as_str())? {
                return Ok(());
            }

            // Warn on duplicate Lua registrations.
            let subagents: Table = lua.globals().get::<Table>("bone")?.get("_subagents")?;
            for entry in subagents.clone().pairs() {
                let (_key, entry_table): (u64, Table) = entry?;
                let existing_name: String = entry_table
                    .get("name")
                    .unwrap_or_else(|_| "<missing>".to_string());
                if existing_name == name {
                    crate::ext::ctx::runtime_warn_once(format!(
                        "bone-lua warn: register_subagent: duplicate name '{name}'; skipping"
                    ));
                    return Ok(());
                }
            }

            // Store the entry as-is; the subagent tool reads fields as needed.
            subagents.push(args)?;
            Ok(())
        })
        .map_err(crate::util::errstr)?;

    let subagent = lua.create_table().map_err(crate::util::errstr)?;
    subagent
        .set("register", register_fn)
        .map_err(crate::util::errstr)?;

    let list_settings = Arc::clone(&settings);
    let list = lua
        .create_function(move |lua, ()| {
            let resolved = list_settings
                .lock()
                .map_err(|e| mlua::Error::external(format!("settings lock poisoned: {e}")))?
                .resolved()
                .clone();
            lua.to_value(&super::types::collect_subagents(&resolved, lua))
        })
        .map_err(crate::util::errstr)?;
    subagent.set("list", list).map_err(crate::util::errstr)?;

    let upsert_settings = Arc::clone(&settings);
    let upsert = lua
        .create_function(move |lua, value: Value| {
            let mut agent: bone_protocol::SubagentDefinition = lua.from_value(value)?;
            agent.source = "config".into();
            upsert_settings
                .lock()
                .map_err(|e| mlua::Error::external(format!("settings lock poisoned: {e}")))?
                .upsert_subagent(agent)
                .map_err(mlua::Error::external)
        })
        .map_err(crate::util::errstr)?;
    subagent
        .set("upsert", upsert)
        .map_err(crate::util::errstr)?;

    let delete_settings = Arc::clone(&settings);
    let delete = lua
        .create_function(move |lua, name: String| {
            reject_lua_only(lua, &delete_settings, &name)?;
            delete_settings
                .lock()
                .map_err(|e| mlua::Error::external(format!("settings lock poisoned: {e}")))?
                .delete_subagent(&name)
                .map_err(mlua::Error::external)
        })
        .map_err(crate::util::errstr)?;
    subagent
        .set("delete", delete)
        .map_err(crate::util::errstr)?;

    let enabled_settings = Arc::clone(&settings);
    let set_enabled = lua
        .create_function(move |lua, (name, enabled): (String, bool)| {
            let resolved = enabled_settings
                .lock()
                .map_err(|e| mlua::Error::external(format!("settings lock poisoned: {e}")))?
                .resolved()
                .clone();
            let lua_agent = super::types::collect_subagents(&resolved, lua)
                .into_iter()
                .find(|agent| agent.name == name && agent.source == "lua");
            let mut settings = enabled_settings
                .lock()
                .map_err(|e| mlua::Error::external(format!("settings lock poisoned: {e}")))?;
            if let Some(mut agent) = lua_agent {
                agent.enabled = enabled;
                agent.source = "config".into();
                settings.upsert_subagent(agent)
            } else {
                settings.set_subagent_enabled(&name, enabled)
            }
            .map_err(mlua::Error::external)
        })
        .map_err(crate::util::errstr)?;
    subagent
        .set("set_enabled", set_enabled)
        .map_err(crate::util::errstr)?;

    bone.set("subagent", subagent)
        .map_err(crate::util::errstr)?;
    Ok(())
}

fn reject_lua_only(lua: &Lua, settings: &Arc<Mutex<Settings>>, name: &str) -> mlua::Result<()> {
    let resolved = settings
        .lock()
        .map_err(|e| mlua::Error::external(format!("settings lock poisoned: {e}")))?
        .resolved()
        .clone();
    if super::types::collect_subagents(&resolved, lua)
        .iter()
        .any(|agent| agent.name == name && agent.source == "lua")
    {
        return Err(mlua::Error::external(format!(
            "Lua-defined sub-agent '{name}' is read-only; edit init.lua"
        )));
    }
    Ok(())
}

/// Seed enabled canonical YAML definitions before `init.lua` runs. Config wins
/// duplicate names; later matching Lua registrations are silently ignored.
pub(crate) fn register_config_subagents(lua: &Lua, settings: &BoneSettings) -> Result<(), String> {
    let bone: Table = lua.globals().get("bone").map_err(crate::util::errstr)?;
    let subagent: Table = bone.get("subagent").map_err(crate::util::errstr)?;
    let register: mlua::Function = subagent.get("register").map_err(crate::util::errstr)?;
    for (name, config) in settings.subagents.iter().filter(|(_, agent)| agent.enabled) {
        let entry = lua.create_table().map_err(crate::util::errstr)?;
        entry
            .set("name", name.as_str())
            .map_err(crate::util::errstr)?;
        entry
            .set("_source", "config")
            .map_err(crate::util::errstr)?;
        entry
            .set("description", config.description.as_str())
            .map_err(crate::util::errstr)?;
        if let Some(value) = &config.system_prompt {
            entry
                .set("system_prompt", value.as_str())
                .map_err(crate::util::errstr)?;
        }
        if let Some(value) = &config.provider {
            entry
                .set("provider", value.as_str())
                .map_err(crate::util::errstr)?;
        }
        if let Some(value) = &config.model {
            entry
                .set("model", value.as_str())
                .map_err(crate::util::errstr)?;
        }
        entry
            .set("approval", config.approval.as_str())
            .map_err(crate::util::errstr)?;
        if let Some(value) = config.timeout_ms {
            entry
                .set("timeout_ms", value)
                .map_err(crate::util::errstr)?;
        }
        if let Some(value) = config.max_concurrency {
            entry
                .set("max_concurrency", value)
                .map_err(crate::util::errstr)?;
        }
        register.call::<()>(entry).map_err(crate::util::errstr)?;
    }
    let configured: Table = bone
        .get("_config_subagent_names")
        .map_err(crate::util::errstr)?;
    for name in settings.subagents.keys() {
        configured
            .set(name.as_str(), true)
            .map_err(crate::util::errstr)?;
    }
    Ok(())
}

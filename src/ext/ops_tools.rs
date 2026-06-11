//! `bone.register_tool(table)` — validates and stores a Lua tool definition
//! in `bone._tools` for later collection by the Rust boot code.

use mlua::{Lua, Table};

/// Create the `bone.register_tool` function and the `bone._tools` storage array.
pub(crate) fn setup_register_tool(lua: &Lua, bone: &Table) -> Result<(), String> {
    let tools_array = lua.create_table().map_err(|e| e.to_string())?;
    bone.set("_tools", tools_array).map_err(|e| e.to_string())?;

    let register_fn = lua
        .create_function(|lua, args: Table| {
            // Lightweight check: name must be present so warnings are attributable.
            // Full validation of description, parameters, safety, and execute
            // happens in LuaTool::from_entry during tool collection.
            if args.get::<String>("name").is_err() {
                eprintln!("bone-lua warn: register_tool: missing or invalid 'name'; skipping");
                return Ok(());
            }

            // Store the entry as-is; from_entry validates the rest.
            let tools: Table = lua.globals().get::<Table>("bone")?.get("_tools")?;
            tools.push(args)?;
            Ok(())
        })
        .map_err(|e| e.to_string())?;

    bone.set("register_tool", register_fn)
        .map_err(|e| e.to_string())?;
    Ok(())
}

// `bone.register_subagent(table)` — validates and stores a sub-agent definition
// in `bone._subagents` for later use by the subagent tool.

/// Create the `bone.register_subagent` function and the `bone._subagents` storage table.
pub(crate) fn setup_register_subagent(lua: &Lua, bone: &Table) -> Result<(), String> {
    let subagents_table = lua.create_table().map_err(|e| e.to_string())?;
    bone.set("_subagents", subagents_table)
        .map_err(|e| e.to_string())?;

    let register_fn = lua
        .create_function(|lua, args: Table| {
            let name: String = match args.get("name") {
                Ok(n) => n,
                Err(_) => {
                    eprintln!(
                        "bone-lua warn: register_subagent: missing or invalid 'name'; skipping"
                    );
                    return Ok(());
                }
            };
            if name.is_empty() {
                eprintln!("bone-lua warn: register_subagent: empty 'name'; skipping");
                return Ok(());
            }

            // Warn on duplicate name.
            let subagents: Table = lua.globals().get::<Table>("bone")?.get("_subagents")?;
            for entry in subagents.clone().pairs() {
                let (_key, entry_table): (u64, Table) = entry?;
                let existing_name: String = entry_table
                    .get("name")
                    .unwrap_or_else(|_| "<missing>".to_string());
                if existing_name == name {
                    eprintln!(
                        "bone-lua warn: register_subagent: duplicate name '{name}'; skipping"
                    );
                    return Ok(());
                }
            }

            // Store the entry as-is; the subagent tool reads fields as needed.
            subagents.push(args)?;
            Ok(())
        })
        .map_err(|e| e.to_string())?;

    bone.set("register_subagent", register_fn)
        .map_err(|e| e.to_string())?;
    Ok(())
}

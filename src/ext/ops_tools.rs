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

//! `bone.register_tool(table)` — validates and stores a Lua tool definition
//! in `bone._tools` for later collection by the Rust boot code.

use mlua::{Lua, Table, Value};

/// Create the `bone.register_tool` function and the `bone._tools` storage array.
pub(crate) fn setup_register_tool(lua: &Lua, bone: &Table) -> Result<(), String> {
    let tools_array = lua.create_table().map_err(|e| e.to_string())?;
    bone.set("_tools", tools_array)
        .map_err(|e| e.to_string())?;

    let register_fn = lua
        .create_function(|lua, args: Table| {
            let tools: Table = lua.globals().get::<Table>("bone")?.get("_tools")?;

            // Validate required fields.
            let name: String = match args.get("name") {
                Ok(Value::String(s)) => s.to_str()?.to_string(),
                _ => {
                    eprintln!("bone-lua warn: register_tool: missing or invalid 'name'; skipping");
                    return Ok(());
                }
            };

            let _description: String = match args.get("description") {
                Ok(Value::String(s)) => s.to_str()?.to_string(),
                _ => {
                    eprintln!("bone-lua warn: register_tool '{name}': missing or invalid 'description'; skipping");
                    return Ok(());
                }
            };

            // parameters must be a table (JSON Schema object)
            match args.get("parameters") {
                Ok(Value::Table(_)) => {}
                _ => {
                    eprintln!("bone-lua warn: register_tool '{name}': missing or invalid 'parameters'; skipping");
                    return Ok(());
                }
            }

            let safety_str: String = match args.get("safety") {
                Ok(Value::String(s)) => s.to_str()?.to_string(),
                _ => {
                    eprintln!("bone-lua warn: register_tool '{name}': missing or invalid 'safety'; skipping");
                    return Ok(());
                }
            };
            if !matches!(safety_str.as_str(), "read_only" | "danger") {
                eprintln!("bone-lua warn: register_tool '{name}': safety must be 'read_only' or 'danger'; skipping");
                return Ok(());
            }

            // execute must be a function
            match args.get("execute") {
                Ok(Value::Function(_)) => {}
                _ => {
                    eprintln!("bone-lua warn: register_tool '{name}': missing or invalid 'execute' function; skipping");
                    return Ok(());
                }
            }

            // Store the validated entry as-is.
            tools.push(args)?;
            Ok(())
        })
        .map_err(|e| e.to_string())?;

    bone.set("register_tool", register_fn)
        .map_err(|e| e.to_string())?;
    Ok(())
}

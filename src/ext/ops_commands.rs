//! `bone.register_command(name, def)` — validates and stores a Lua command definition
//! in `bone._commands` for later collection by the Rust boot code.
//!
//! `def` can be either:
//!   - A table with `description` and `handler` fields (long form)
//!   - A function (short form — description defaults to empty string)

use mlua::{Lua, Table, Value, Variadic};

/// A command registered from Lua via `bone.register_command()`.
#[derive(Clone)]
pub struct RegisteredLuaCommand {
    pub name: String,
    pub description: String,
}

/// Create the `bone.register_command` function and the `bone._commands` storage array.
pub(crate) fn setup_register_command(lua: &Lua, bone: &Table) -> Result<(), String> {
    let commands_array = lua.create_table().map_err(|e| e.to_string())?;
    bone.set("_commands", commands_array)
        .map_err(|e| e.to_string())?;

    let register_fn = lua
        .create_function(|lua, args: Variadic<Value>| {
            let commands: Table = lua.globals().get::<Table>("bone")?.get("_commands")?;
            let Some(first) = args.first() else {
                eprintln!("bone-lua warn: register_command: missing name; skipping");
                return Ok(());
            };
            let name = match first {
                Value::String(s) => s.to_str()?.to_string(),
                _ => {
                    eprintln!("bone-lua warn: register_command: missing or invalid name; skipping");
                    return Ok(());
                }
            };
            let Some(def) = args.get(1) else {
                eprintln!("bone-lua warn: register_command '{name}': missing handler; skipping");
                return Ok(());
            };

            let entry = lua.create_table()?;
            entry.set("name", name.as_str())?;
            match def {
                Value::Function(f) => {
                    entry.set("description", "")?;
                    entry.set("handler", f.clone())?;
                }
                Value::Table(t) => {
                    let handler = match t.get::<Value>("handler") {
                        Ok(Value::Function(f)) => f,
                        Ok(_) => {
                            eprintln!("bone-lua warn: register_command '{name}': handler is not a function; skipping");
                            return Ok(());
                        }
                        Err(_) => {
                            eprintln!("bone-lua warn: register_command '{name}': missing handler; skipping");
                            return Ok(());
                        }
                    };
                    let description = t.get::<String>("description").unwrap_or_default();
                    entry.set("description", description)?;
                    entry.set("handler", handler)?;
                }
                _ => {
                    eprintln!("bone-lua warn: register_command '{name}': handler must be a function or table; skipping");
                    return Ok(());
                }
            }

            commands.push(entry)?;
            Ok(())
        })
        .map_err(|e| e.to_string())?;

    bone.set("register_command", register_fn)
        .map_err(|e| e.to_string())?;
    Ok(())
}

//! `bone.register_command(name, def)` — validates and stores a Lua command definition
//! in `bone._commands` for later collection by the Rust boot code.
//!
//! `def` can be either:
//!   - A table with `description` and `handler` fields (long form)
//!   - A function (short form — description defaults to empty string)

use mlua::{Lua, Table, Value, Variadic};

/// Look up a command handler by name from `bone._commands`.
/// Returns the handler as a `mlua::Function` if found.
pub fn find_handler(lua: &Lua, name: &str) -> Option<mlua::Function> {
    let bone_table = lua.globals().get::<Table>("bone").ok()?;
    let commands_table = bone_table.get::<Table>("_commands").ok()?;

    for entry in commands_table.sequence_values::<Table>() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let cmd_name: String = match entry.get::<Value>("name") {
            Ok(Value::String(s)) => match s.to_str() {
                Ok(s) => s.to_string(),
                Err(_) => continue,
            },
            _ => continue,
        };
        if cmd_name != name {
            continue;
        }

        let handler: Value = entry.get("handler").ok()?;
        return match handler {
            Value::Function(f) => Some(f),
            Value::Table(t) => t.get("handler").ok().and_then(|v| match v {
                Value::Function(f) => Some(f),
                _ => None,
            }),
            _ => None,
        };
    }
    None
}

/// A command registered from Lua via `bone.register_command()`.
#[derive(Clone)]
pub struct RegisteredLuaCommand {
    pub name: String,
    pub description: String,
}

/// Create the `bone.register_command` function and the `bone._commands` storage array.
pub(crate) fn setup_register_command(lua: &Lua, bone: &Table) -> Result<(), String> {
    let commands_array = lua.create_table().map_err(crate::util::errstr)?;
    bone.set("_commands", commands_array)
        .map_err(crate::util::errstr)?;

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
        .map_err(crate::util::errstr)?;

    bone.set("register_command", register_fn)
        .map_err(crate::util::errstr)?;
    Ok(())
}

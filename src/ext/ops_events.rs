/// `bone.on(event_name, handler)` — registers an event handler.
///
/// Valid event names: session_start, session_end, message, tool_call,
/// tool_result, mode_change. Handlers are stored in `bone._handlers[name]`
/// as an ordered array and called in registration order.

use mlua::{Lua, Table};

const EVENT_NAMES: &[&str] = &[
    "session_start",
    "session_end",
    "message",
    "tool_call",
    "tool_result",
    "mode_change",
];

/// Create the `bone.on` function and the `bone._handlers` storage table.
pub(crate) fn setup_on(lua: &Lua, bone: &Table) -> Result<(), String> {
    let handlers = lua.create_table().map_err(|e| e.to_string())?;
    for &name in EVENT_NAMES {
        let array = lua.create_table().map_err(|e| e.to_string())?;
        handlers.set(name, array).map_err(|e| e.to_string())?;
    }
    bone.set("_handlers", handlers)
        .map_err(|e| e.to_string())?;

    let on_fn = lua
        .create_function(|lua, (event_name, handler): (String, mlua::Function)| {
            let bone: Table = lua.globals().get("bone")?;
            let handlers: Table = bone.get("_handlers")?;
            match handlers.get::<Option<Table>>(&*event_name)? {
                Some(event_handlers) => {
                    event_handlers.push(handler)?;
                }
                None => {
                    eprintln!(
                        "bone-lua warn: bone.on: unknown event '{event_name}'; ignoring"
                    );
                }
            }
            Ok(())
        })
        .map_err(|e| e.to_string())?;

    bone.set("on", on_fn).map_err(|e| e.to_string())?;
    Ok(())
}

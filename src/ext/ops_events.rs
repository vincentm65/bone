//! Lua binding for `bone.on` / `bone.api.autocmd` event-handler registration.

/// `bone.on(event_name, handler)` — registers an event handler (autocmd).
///
/// The built-in event names below are pre-seeded, but **any** name is accepted:
/// an unknown name creates its handler array on demand. This makes `bone.on`
/// (and its alias `bone.api.autocmd`) a general autocmd registry — Lua plugins
/// can define custom events and fire them with `bone.api.emit(name, payload)`,
/// or Rust can drive them via `ExtensionManager::dispatch_simple(name, ...)`.
/// Handlers are stored in `bone._handlers[name]` as an ordered array and called
/// in registration order.
use mlua::{Lua, Table};

const EVENT_NAMES: &[&str] = &[
    "session_start",
    "session_end",
    "message",
    "tool_call",
    "tool_result",
    "mode_change",
    "before_turn",
    "turn_start",
    "turn_end",
    "token_usage",
];

/// Create the `bone.on` function and the `bone._handlers` storage table.
pub(crate) fn setup_on(lua: &Lua, bone: &Table) -> Result<(), String> {
    let handlers = lua.create_table().map_err(crate::util::errstr)?;
    for &name in EVENT_NAMES {
        let array = lua.create_table().map_err(crate::util::errstr)?;
        handlers.set(name, array).map_err(crate::util::errstr)?;
    }
    bone.set("_handlers", handlers)
        .map_err(crate::util::errstr)?;

    let on_fn = lua
        .create_function(|lua, (event_name, handler): (String, mlua::Function)| {
            let bone: Table = lua.globals().get("bone")?;
            let handlers: Table = bone.get("_handlers")?;
            match handlers.get::<Option<Table>>(&*event_name)? {
                Some(event_handlers) => {
                    event_handlers.push(handler)?;
                }
                None => {
                    // Unknown event name → create the array on demand (autocmd).
                    let array = lua.create_table()?;
                    array.push(handler)?;
                    handlers.set(&*event_name, array)?;
                }
            }
            Ok(())
        })
        .map_err(crate::util::errstr)?;

    bone.set("on", on_fn).map_err(crate::util::errstr)?;
    Ok(())
}

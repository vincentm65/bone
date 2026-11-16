//! UI libraries for Lua (`/tui`): which library is in use, and making,
//! choosing and deleting one.

use bone_lua::to_lua;
use mlua::{Lua, MultiValue};

use super::{Host, args, err, ret};

/// The ops; `Err` gives the arguments back for an op that is not one.
pub fn dispatch<H: Host>(
    h: &mut H,
    lua: &Lua,
    op: &str,
    a: MultiValue,
) -> Result<mlua::Result<MultiValue>, MultiValue> {
    let dir = h.config_dir().map(std::path::Path::to_owned);
    let need_dir = || {
        dir.clone()
            .ok_or_else(|| err("there is no config directory"))
    };
    Ok(match op {
        "library_create" | "library_delete" => {
            args::<(String, Option<String>)>(lua, a).and_then(|(name, from)| {
                let dir = need_dir()?;
                let path = if op == "library_create" {
                    bone_lua::create_tui_library(&dir, &name, from.as_deref().unwrap_or(""), h.side())
                } else {
                    bone_lua::delete_tui_library(&dir, &name)
                };
                ret(lua, path.map_err(err)?.to_string_lossy().into_owned())
            })
        }
        "library_info" => {
            let dir = dir.as_deref();
            let info = serde_json::json!({
                "active": dir.and_then(bone_lua::tui_library).unwrap_or_else(|| "standard".into()),
                // What this frontend draws with (the GUI leaves out a
                // library made for the terminal).
                "drawn": dir.and_then(|d| bone_lua::library(d, h.side())).unwrap_or_else(|| "standard".into()),
                "custom": dir.map(bone_lua::tui_libraries).unwrap_or_default(),
                "dir": dir.map(|d| d.join("libraries").to_string_lossy().into_owned()),
            });
            to_lua(lua, &info).and_then(|v| ret(lua, v))
        }
        "library_use" => args::<String>(lua, a).and_then(|name| {
            let dir = need_dir()?;
            let prev = bone_lua::tui_library(&dir).unwrap_or_else(|| "standard".into());
            bone_lua::set_tui_library(&dir, &name).map_err(err)?;
            h.library_changed(prev);
            ret(lua, ())
        }),
        _ => return Err(a),
    })
}

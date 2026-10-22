//! UI libraries and runtime overrides for Lua (`/tui`, `/runtime`): which
//! library is in use, making, choosing and deleting one, and the runtime
//! files the config dir replaces.

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
                    bone_lua::create_tui_library(&dir, &name, from.as_deref().unwrap_or(""))
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
        "runtime_overrides" => {
            let list: Vec<serde_json::Value> = (dir.as_deref())
                .map(bone_lua::runtime_overrides)
                .unwrap_or_default()
                .into_iter()
                .map(|(path, same)| serde_json::json!({ "path": path, "same": same }))
                .collect();
            to_lua(lua, &serde_json::Value::Array(list)).and_then(|v| ret(lua, v))
        }
        "runtime_reset" => args::<(String, Option<String>)>(lua, a).and_then(|(rel, stamp)| {
            let dir = need_dir()?;
            // One backup folder per reset (or per `/runtime reset all`).
            let stamp = stamp.unwrap_or_else(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs())
                    .to_string()
            });
            let backup = dir.join("runtime-backup").join(&stamp);
            let to = bone_lua::reset_runtime_file(&dir, &rel, &backup).map_err(err)?;
            ret(lua, to.to_string_lossy().into_owned())
        }),
        _ => return Err(a),
    })
}

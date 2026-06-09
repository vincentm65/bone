//! `bone.plugin` API — explicit Lua plugin management.
//!
//! Plugins live in `<config_dir>/lua/plugins/<name>/init.lua`.
//! Installing does not auto-load; the user must call `bone.plugin.load("name")`
//! from `init.lua`.

use std::path::PathBuf;

use mlua::{Lua, Table, Value};

/// Plugin directory relative to config dir.
fn plugins_dir(config_dir: &str) -> PathBuf {
    PathBuf::from(config_dir).join("lua/plugins")
}

/// Set up the `bone.plugin` table on the Lua state.
pub(crate) fn setup_plugin(lua: &Lua, bone: &Table) -> Result<(), String> {
    let plugin_table = lua.create_table().map_err(|e| e.to_string())?;

    // Internal set of loaded plugin names to prevent double-loading.
    let loaded_set = lua.create_table().map_err(|e| e.to_string())?;
    bone.set("_loaded_plugins", loaded_set)
        .map_err(|e| e.to_string())?;

    // bone.plugin.load("name")
    let load_fn = lua
        .create_function(|lua, name: String| {
            let bone: Table = lua.globals().get::<Table>("bone")?;
            let loaded: Table = bone.get::<Table>("_loaded_plugins")?;

            // Already loaded — no-op.
            if loaded.get::<Option<bool>>(&*name)?.unwrap_or(false) {
                return Ok(false);
            }

            let config_dir: String = bone.get::<String>("config_dir")?;
            let init_path = plugins_dir(&config_dir)
                .join(&name)
                .join("init.lua");

            if !init_path.exists() {
                eprintln!(
                    "bone: warning: plugin '{}' not found at {}",
                    name,
                    init_path.display()
                );
                return Ok(false);
            }

            let source = match std::fs::read_to_string(&init_path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!(
                        "bone: warning: plugin '{}': failed to read {}: {e}",
                        name,
                        init_path.display()
                    );
                    return Ok(false);
                }
            };

            match lua
                .load(&source)
                .set_name(format!("plugins/{name}/init.lua"))
                .exec()
            {
                Ok(()) => {
                    loaded.set(name.as_str(), true)?;
                    Ok(true)
                }
                Err(e) => {
                    eprintln!("bone: warning: plugin '{}' error: {e}", name);
                    Ok(false)
                }
            }
        })
        .map_err(|e| e.to_string())?;
    plugin_table
        .set("load", load_fn)
        .map_err(|e| e.to_string())?;

    // bone.plugin.install("user/repo") or bone.plugin.install("/local/path")
    let install_fn = lua
        .create_function(|lua, source: String| {
            let bone: Table = lua.globals().get::<Table>("bone")?;
            let config_dir: String = bone.get::<String>("config_dir")?;
            let dir = plugins_dir(&config_dir);

            // Ensure plugins directory exists.
            let _ = std::fs::create_dir_all(&dir);

            if source.starts_with('/') || source.starts_with('.') {
                // Local path — create a symlink.
                let src = std::path::Path::new(&source);
                let name = src
                    .file_name()
                    .ok_or_else(|| mlua::Error::external("invalid local path"))?
                    .to_string_lossy()
                    .to_string();
                let dest = dir.join(&name);
                if dest.exists() {
                    return Err(mlua::Error::external(format!(
                        "plugin '{}' already exists",
                        name
                    )));
                }
                let abs = if source.starts_with('.') {
                    let cwd: String = bone.get::<String>("cwd")?;
                    std::path::Path::new(&cwd).join(&source)
                } else {
                    src.to_path_buf()
                };
                std::os::unix::fs::symlink(&abs, &dest).map_err(|e| {
                    mlua::Error::external(format!("symlink failed: {e}"))
                })?;
                Ok(name)
            } else {
                // GitHub-style "user/repo" — git clone.
                let name = source
                    .rsplit('/')
                    .next()
                    .ok_or_else(|| mlua::Error::external("invalid repo path"))?
                    .to_string();
                let dest = dir.join(&name);
                if dest.exists() {
                    return Err(mlua::Error::external(format!(
                        "plugin '{}' already exists",
                        name
                    )));
                }
                let url = format!("https://github.com/{source}");
                let output = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        tokio::process::Command::new("git")
                            .args(["clone", &url])
                            .arg(&dest)
                            .stdout(std::process::Stdio::piped())
                            .stderr(std::process::Stdio::piped())
                            .status()
                            .await
                    })
                });
                match output {
                    Ok(status) if status.success() => Ok(name),
                    Ok(status) => Err(mlua::Error::external(format!(
                        "git clone failed (exit {})",
                        status.code().unwrap_or(-1)
                    ))),
                    Err(e) => Err(mlua::Error::external(format!(
                        "git clone failed: {e}"
                    ))),
                }
            }
        })
        .map_err(|e| e.to_string())?;
    plugin_table
        .set("install", install_fn)
        .map_err(|e| e.to_string())?;

    // bone.plugin.remove("name")
    let remove_fn = lua
        .create_function(|lua, name: String| {
            let bone: Table = lua.globals().get::<Table>("bone")?;
            let config_dir: String = bone.get::<String>("config_dir")?;
            let dir = plugins_dir(&config_dir).join(&name);
            if !dir.exists() {
                return Err(mlua::Error::external(format!(
                    "plugin '{}' not found",
                    name
                )));
            }
            std::fs::remove_dir_all(&dir).map_err(|e| {
                mlua::Error::external(format!("remove failed: {e}"))
            })?;

            // Clear loaded flag.
            let loaded: Table = bone.get::<Table>("_loaded_plugins")?;
            loaded.set(name.as_str(), Value::Nil)?;

            Ok(true)
        })
        .map_err(|e| e.to_string())?;
    plugin_table
        .set("remove", remove_fn)
        .map_err(|e| e.to_string())?;

    // bone.plugin.list() → table of { name = { has_init = bool } }
    let list_fn = lua
        .create_function(|lua, ()| {
            let bone: Table = lua.globals().get::<Table>("bone")?;
            let config_dir: String = bone.get::<String>("config_dir")?;
            let dir = plugins_dir(&config_dir);
            let result = lua.create_table()?;
            if !dir.is_dir() {
                return Ok(result);
            }
            let mut entries: Vec<_> = std::fs::read_dir(&dir)
                .unwrap_or_else(|e| panic!("failed to read plugins dir: {e}"))
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect();
            entries.sort();
            for path in entries {
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                let has_init = path.join("init.lua").exists();
                let info = lua.create_table()?;
                info.set("has_init", has_init)?;
                result.set(name.as_str(), info)?;
            }
            Ok(result)
        })
        .map_err(|e| e.to_string())?;
    plugin_table
        .set("list", list_fn)
        .map_err(|e| e.to_string())?;

    // bone.plugin.update("name")
    let update_fn = lua
        .create_function(|lua, name: String| {
            let bone: Table = lua.globals().get::<Table>("bone")?;
            let config_dir: String = bone.get::<String>("config_dir")?;
            let dir = plugins_dir(&config_dir).join(&name);
            if !dir.is_dir() {
                return Err(mlua::Error::external(format!(
                    "plugin '{}' not found",
                    name
                )));
            }
            // Check it's a git repo.
            if !dir.join(".git").exists() {
                return Err(mlua::Error::external(format!(
                    "plugin '{}' is not a git repository",
                    name
                )));
            }
            let output = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    tokio::process::Command::new("git")
                        .args(["pull"])
                        .current_dir(&dir)
                        .stdout(std::process::Stdio::piped())
                        .stderr(std::process::Stdio::piped())
                        .status()
                        .await
                })
            });
            match output {
                Ok(status) if status.success() => Ok(true),
                Ok(status) => Err(mlua::Error::external(format!(
                    "git pull failed (exit {})",
                    status.code().unwrap_or(-1)
                ))),
                Err(e) => Err(mlua::Error::external(format!(
                    "git pull failed: {e}"
                ))),
            }
        })
        .map_err(|e| e.to_string())?;
    plugin_table
        .set("update", update_fn)
        .map_err(|e| e.to_string())?;

    bone.set("plugin", plugin_table)
        .map_err(|e| e.to_string())?;
    Ok(())
}

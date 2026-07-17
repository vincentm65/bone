//! `bone.plugin` API — explicit Lua plugin management.
//!
//! Plugins live in `<config_dir>/lua/plugins/<name>/init.lua`.
//! Installing does not auto-load; the user must call `bone.plugin.load("name")`
//! from `init.lua`.

use std::path::{Path, PathBuf};

use mlua::{Lua, Table, Value};

/// Plugin directory relative to config dir.
fn plugins_dir(config_dir: &str) -> PathBuf {
    PathBuf::from(config_dir).join("lua/plugins")
}

fn validate_plugin_name(name: &str) -> Result<(), mlua::Error> {
    if super::is_safe_leaf_name(name) {
        Ok(())
    } else {
        Err(mlua::Error::external(format!(
            "invalid plugin name '{name}': expected one directory name"
        )))
    }
}

#[cfg(unix)]
fn symlink_plugin_dir(src: &Path, dest: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src, dest)
}

#[cfg(windows)]
fn symlink_plugin_dir(src: &Path, dest: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(src, dest)
}

fn run_git(args: &[&str], cwd: Option<&Path>, verb: &str) -> Result<(), mlua::Error> {
    let mut command = tokio::process::Command::new("git");
    command
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    match super::ctx::block_on(command.status()) {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(mlua::Error::external(format!(
            "git {verb} failed (exit {})",
            status.code().unwrap_or(-1)
        ))),
        Err(e) => Err(mlua::Error::external(format!("git {verb} failed: {e}"))),
    }
}

/// Set up the `bone.plugin` table on the Lua state.
pub(crate) fn setup_plugin(lua: &Lua, bone: &Table) -> Result<(), String> {
    let plugin_table = lua.create_table().map_err(crate::util::errstr)?;

    // Internal set of loaded plugin names to prevent double-loading.
    let loaded_set = lua.create_table().map_err(crate::util::errstr)?;
    bone.set("_loaded_plugins", loaded_set)
        .map_err(crate::util::errstr)?;

    // bone.plugin.load("name")
    let load_fn = lua
        .create_function(|lua, name: String| {
            validate_plugin_name(&name)?;
            let bone: Table = lua.globals().get::<Table>("bone")?;
            let loaded: Table = bone.get::<Table>("_loaded_plugins")?;

            // Already loaded — no-op.
            if loaded.get::<Option<bool>>(&*name)?.unwrap_or(false) {
                return Ok(false);
            }

            let config_dir: String = bone.get::<String>("config_dir")?;
            let init_path = plugins_dir(&config_dir).join(&name).join("init.lua");

            if !init_path.exists() {
                crate::ext::ctx::runtime_warn_once(format!(
                    "bone: warning: plugin '{}' not found at {}",
                    name,
                    init_path.display()
                ));
                return Ok(false);
            }

            let source = match std::fs::read_to_string(&init_path) {
                Ok(s) => s,
                Err(e) => {
                    crate::ext::ctx::runtime_warn_once(format!(
                        "bone: warning: plugin '{}': failed to read {}: {e}",
                        name,
                        init_path.display()
                    ));
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
                    crate::ext::ctx::runtime_warn(format!(
                        "bone: warning: plugin '{}' error: {e}",
                        name
                    ));
                    Ok(false)
                }
            }
        })
        .map_err(crate::util::errstr)?;
    plugin_table
        .set("load", load_fn)
        .map_err(crate::util::errstr)?;

    // bone.plugin.install("user/repo") or bone.plugin.install("/local/path")
    let install_fn = lua
        .create_function(|lua, source: String| {
            let bone: Table = lua.globals().get::<Table>("bone")?;
            let config_dir: String = bone.get::<String>("config_dir")?;
            let dir = plugins_dir(&config_dir);

            // Ensure plugins directory exists.
            let _ = std::fs::create_dir_all(&dir);

            let is_local = source.starts_with('/') || source.starts_with('.');
            let name = if is_local {
                Path::new(&source)
                    .file_name()
                    .ok_or_else(|| mlua::Error::external("invalid local path"))?
                    .to_string_lossy()
                    .to_string()
            } else {
                source
                    .rsplit('/')
                    .next()
                    .ok_or_else(|| mlua::Error::external("invalid repo path"))?
                    .to_string()
            };
            validate_plugin_name(&name)?;
            let dest = dir.join(&name);
            if dest.exists() {
                return Err(mlua::Error::external(format!(
                    "plugin '{}' already exists",
                    name
                )));
            }

            if is_local {
                // Local path — create a symlink.
                let abs = if source.starts_with('.') {
                    let cwd: String = bone.get::<String>("cwd")?;
                    Path::new(&cwd).join(&source)
                } else {
                    Path::new(&source).to_path_buf()
                };
                symlink_plugin_dir(&abs, &dest)
                    .map_err(|e| mlua::Error::external(format!("symlink failed: {e}")))?;
                Ok(name)
            } else {
                // GitHub-style "user/repo" — git clone.
                let url = format!("https://github.com/{source}");
                let dest = dest.to_string_lossy();
                run_git(&["clone", &url, &dest], None, "clone")?;
                Ok(name)
            }
        })
        .map_err(crate::util::errstr)?;
    plugin_table
        .set("install", install_fn)
        .map_err(crate::util::errstr)?;

    // bone.plugin.remove("name")
    let remove_fn = lua
        .create_function(|lua, name: String| {
            validate_plugin_name(&name)?;
            let bone: Table = lua.globals().get::<Table>("bone")?;
            let config_dir: String = bone.get::<String>("config_dir")?;
            let dir = plugins_dir(&config_dir).join(&name);
            if !dir.exists() {
                return Err(mlua::Error::external(format!(
                    "plugin '{}' not found",
                    name
                )));
            }
            std::fs::remove_dir_all(&dir)
                .map_err(|e| mlua::Error::external(format!("remove failed: {e}")))?;

            // Clear loaded flag.
            let loaded: Table = bone.get::<Table>("_loaded_plugins")?;
            loaded.set(name.as_str(), Value::Nil)?;

            Ok(true)
        })
        .map_err(crate::util::errstr)?;
    plugin_table
        .set("remove", remove_fn)
        .map_err(crate::util::errstr)?;

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
                .map_err(|e| {
                    mlua::Error::external(format!("failed to read plugins dir {dir:?}: {e}"))
                })?
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
        .map_err(crate::util::errstr)?;
    plugin_table
        .set("list", list_fn)
        .map_err(crate::util::errstr)?;

    // bone.plugin.update("name")
    let update_fn = lua
        .create_function(|lua, name: String| {
            validate_plugin_name(&name)?;
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
            run_git(&["pull"], Some(&dir), "pull")?;
            Ok(true)
        })
        .map_err(crate::util::errstr)?;
    plugin_table
        .set("update", update_fn)
        .map_err(crate::util::errstr)?;

    bone.set("plugin", plugin_table)
        .map_err(crate::util::errstr)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_operations_reject_traversal_names() {
        let lua = Lua::new();
        let bone = lua.create_table().unwrap();
        bone.set("config_dir", "/tmp/bone-plugin-test").unwrap();
        bone.set("cwd", "/tmp").unwrap();
        lua.globals().set("bone", bone.clone()).unwrap();
        setup_plugin(&lua, &bone).unwrap();

        for operation in ["load", "remove", "update"] {
            let result: mlua::Result<Value> = lua
                .load(format!("return bone.plugin.{operation}('../escape')"))
                .eval();
            let error = result.expect_err("traversal name should fail").to_string();
            assert!(
                error.contains("invalid plugin name"),
                "unexpected {operation} error: {error}"
            );
        }

        let install: mlua::Result<Value> = lua.load("return bone.plugin.install('user/..')").eval();
        assert!(
            install
                .expect_err("traversal-derived install name should fail")
                .to_string()
                .contains("invalid plugin name")
        );
    }

    #[test]
    fn plugin_names_are_single_path_components() {
        for invalid in ["", ".", "..", "../x", "x/y", r"x\y", "x\0y"] {
            assert!(
                validate_plugin_name(invalid).is_err(),
                "accepted {invalid:?}"
            );
        }
        assert!(validate_plugin_name("example.nvim").is_ok());
    }
}

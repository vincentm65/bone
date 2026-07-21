//! Canonical always-available Lua runtime APIs.
//!
//! `bone.api.ui` contains low-level drawing primitives. User-facing operations
//! live in purpose-specific namespaces such as `bone.keymap`, `bone.settings`,
//! and `bone.theme`, or directly at `bone.submit`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use mlua::{Function, Lua, LuaSerdeExt, Table, Value};

use crate::config::settings::Settings;

/// Get `bone.api`, creating it if absent (so ordering with `api_ui` is safe).
fn api_table(lua: &Lua, bone: &Table) -> mlua::Result<Table> {
    match bone.get::<Option<Table>>("api")? {
        Some(t) => Ok(t),
        None => {
            let t = lua.create_table()?;
            bone.set("api", &t)?;
            Ok(t)
        }
    }
}

/// Register canonical runtime APIs.
pub fn setup_api(
    lua: &Lua,
    bone: &Table,
    settings: Arc<Mutex<Settings>>,
    registry: super::settings_registry::SharedSettingsRegistry,
    settings_path: PathBuf,
) -> Result<(), String> {
    let api = api_table(lua, bone).map_err(crate::util::errstr)?;

    // bone.api.autocmd = bone.on (general event registration).
    if let Some(on) = bone
        .get::<Option<Function>>("on")
        .map_err(crate::util::errstr)?
    {
        api.set("autocmd", on).map_err(crate::util::errstr)?;
    }

    // bone.api.emit(event, payload?) — synchronously invoke registered handlers.
    let emit = lua
        .create_function(|lua, (event, payload): (String, Option<Table>)| {
            let bone: Table = lua.globals().get("bone")?;
            let handlers: Option<Table> = bone.get::<Option<Table>>("_handlers")?;
            let Some(handlers) = handlers else {
                return Ok(());
            };
            let Some(arr) = handlers.get::<Option<Table>>(&*event)? else {
                return Ok(());
            };
            let payload = match payload {
                Some(p) => p,
                None => lua.create_table()?,
            };
            let ctx = lua.create_table()?;
            for h in arr.sequence_values::<Function>().flatten() {
                // Swallow handler errors so one bad autocmd can't break emit.
                if let Err(e) = h.call::<Value>((payload.clone(), ctx.clone())) {
                    super::ctx::runtime_warn(format!(
                        "bone-lua warn: autocmd '{event}' handler error: {e}"
                    ));
                }
            }
            Ok(())
        })
        .map_err(crate::util::errstr)?;
    api.set("emit", emit).map_err(crate::util::errstr)?;

    // bone.submit(text) — queue a prompt for the frontend to submit, like
    // typed input. Drained between turns (or queued behind the active turn).
    let submit = lua
        .create_function(|_, text: String| {
            if !text.trim().is_empty() {
                crate::ext::inbox::push(text);
            }
            Ok(())
        })
        .map_err(crate::util::errstr)?;
    bone.set("submit", submit).map_err(crate::util::errstr)?;

    // bone.keymap.set(key, rhs) — declarations are exposed to frontends,
    // while rhs callbacks and classification remain daemon-owned.
    let keymap = lua.create_table().map_err(crate::util::errstr)?;
    let callbacks = lua.create_table().map_err(crate::util::errstr)?;
    bone.set("_keymap_callbacks", callbacks)
        .map_err(crate::util::errstr)?;
    let keymap_store = Arc::clone(&settings);
    let set = lua
        .create_function(move |lua, (key, rhs): (String, Value)| {
            if key.is_empty() {
                return Err(mlua::Error::external("keymap key must not be empty"));
            }
            let action = match rhs {
                Value::String(value) => value.to_str()?.to_string(),
                Value::Function(callback) => {
                    static NEXT_CALLBACK: std::sync::atomic::AtomicU64 =
                        std::sync::atomic::AtomicU64::new(1);
                    let id = NEXT_CALLBACK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let id = id.to_string();
                    let bone: Table = lua.globals().get("bone")?;
                    let callbacks: Table = bone.get("_keymap_callbacks")?;
                    callbacks.set(&*id, callback)?;
                    format!("__cb_{id}")
                }
                _ => {
                    return Err(mlua::Error::external(
                        "keymap rhs must be a string or function",
                    ));
                }
            };
            if action.is_empty() {
                return Err(mlua::Error::external("keymap rhs must not be empty"));
            }
            let mut store = keymap_store
                .lock()
                .map_err(|e| mlua::Error::external(format!("settings lock poisoned: {e}")))?;
            let bindings = &mut store.inner.keymaps.bindings;
            if let Some(binding) = bindings.iter_mut().find(|binding| binding.key == key) {
                binding.action = action;
            } else {
                bindings.push(crate::config::settings::KeyBinding { key, action });
            }
            Ok(())
        })
        .map_err(crate::util::errstr)?;
    keymap.set("set", set).map_err(crate::util::errstr)?;
    bone.set("keymap", keymap).map_err(crate::util::errstr)?;

    setup_theme_api(lua, bone, Arc::clone(&settings), &settings_path)?;

    // bone.settings.{define,get,set,reset} — canonical settings plus
    // declarative extension-owned schemas. `register` remains as a compatibility
    // alias for the original declaration-table shape.
    let settings_api = lua.create_table().map_err(crate::util::errstr)?;

    let register_registry = Arc::clone(&registry);
    let register = lua
        .create_function(move |lua, declaration: Table| {
            register_settings_page(lua, declaration, &register_registry)
        })
        .map_err(crate::util::errstr)?;
    settings_api
        .set("register", register)
        .map_err(crate::util::errstr)?;

    let define_registry = Arc::clone(&registry);
    let define = lua
        .create_function(move |lua, (namespace, schema): (String, Table)| {
            schema.set("namespace", namespace)?;
            let fields: Table = schema.get("fields")?;
            let mut declared = fields
                .pairs::<String, Table>()
                .collect::<mlua::Result<Vec<_>>>()?;
            declared.sort_by(|(left, _), (right, _)| left.cmp(right));
            let sequence = lua.create_table()?;
            for (index, (key, field)) in declared.into_iter().enumerate() {
                field.set("key", key)?;
                sequence.set(index + 1, field)?;
            }
            schema.set("fields", sequence)?;
            register_settings_page(lua, schema, &define_registry)
        })
        .map_err(crate::util::errstr)?;
    settings_api
        .set("define", define)
        .map_err(crate::util::errstr)?;

    let rollback_registry = Arc::clone(&registry);
    let rollback = lua
        .create_function(move |_, owner: String| {
            rollback_registry
                .write()
                .map_err(|e| {
                    mlua::Error::external(format!("settings registry lock poisoned: {e}"))
                })?
                .remove_owner(&owner);
            Ok(())
        })
        .map_err(crate::util::errstr)?;
    settings_api
        .set("_rollback_owner", rollback)
        .map_err(crate::util::errstr)?;

    let extension_get_store = Arc::clone(&settings);
    let extension_get_registry = Arc::clone(&registry);
    let extension_get = lua
        .create_function(move |lua, path: String| {
            let store = extension_get_store
                .lock()
                .map_err(|e| mlua::Error::external(format!("settings lock poisoned: {e}")))?;
            let value = extension_get_registry
                .read()
                .map_err(|e| {
                    mlua::Error::external(format!("settings registry lock poisoned: {e}"))
                })?
                .resolve(&store, &path)
                .map_err(mlua::Error::external)?;
            lua.to_value(&value)
        })
        .map_err(crate::util::errstr)?;
    settings_api
        .set("_get_extension", extension_get)
        .map_err(crate::util::errstr)?;

    let pages_registry = Arc::clone(&registry);
    let pages_store = Arc::clone(&settings);
    let pages = lua
        .create_function(move |lua, ()| {
            let registry = pages_registry.read().map_err(|e| {
                mlua::Error::external(format!("settings registry lock poisoned: {e}"))
            })?;
            let settings = pages_store
                .lock()
                .map_err(|e| mlua::Error::external(format!("settings lock poisoned: {e}")))?;
            let pages = registry
                .pages()
                .into_iter()
                .map(|page| {
                    let fields = page
                        .fields
                        .into_iter()
                        .map(|field| {
                            let path = format!("{}.{}", page.namespace, field.key);
                            let value = registry.resolve(&settings, &path).unwrap_or(field.default);
                            serde_json::json!({
                                "key": field.key,
                                "label": field.label,
                                "type": field.field_type,
                                "options": field.options,
                                "value": value,
                                "integer": field.integer,
                                "min": field.min,
                                "max": field.max,
                            })
                        })
                        .collect::<Vec<_>>();
                    serde_json::json!({
                        "namespace": page.namespace,
                        "title": page.title,
                        "fields": fields,
                    })
                })
                .collect::<Vec<_>>();
            lua.to_value(&pages)
        })
        .map_err(crate::util::errstr)?;
    settings_api
        .set("_pages", pages)
        .map_err(crate::util::errstr)?;

    let get_store = Arc::clone(&settings);
    let get = lua
        .create_function(move |lua, path: String| {
            if path == "extensions" || path.starts_with("extensions.") {
                return Err(mlua::Error::external(
                    "extension settings are request-scoped; use ctx.settings.get",
                ));
            }
            let value = get_store
                .lock()
                .map_err(|e| mlua::Error::external(format!("settings lock poisoned: {e}")))?
                .get_path(&path)
                .map_err(|e| mlua::Error::external(e.to_string()))?;
            lua.to_value(&value)
        })
        .map_err(crate::util::errstr)?;
    settings_api.set("get", get).map_err(crate::util::errstr)?;

    let set_store = Arc::clone(&settings);
    let set_path = settings_path.clone();
    let set = lua
        .create_function(move |lua, (path, value): (String, Value)| {
            if path == "extensions" || path.starts_with("extensions.") {
                return Err(mlua::Error::external(
                    "extension settings are read-only from Lua commands",
                ));
            }
            let value: serde_json::Value = lua.from_value(value)?;
            set_store
                .lock()
                .map_err(|e| mlua::Error::external(format!("settings lock poisoned: {e}")))?
                .set_path_at(&path, value, &set_path)
                .map_err(|e| mlua::Error::external(e.to_string()))?;
            Ok(())
        })
        .map_err(crate::util::errstr)?;
    settings_api.set("set", set).map_err(crate::util::errstr)?;

    let reset_store = Arc::clone(&settings);
    let reset = lua
        .create_function(move |lua, path: String| {
            if path == "extensions" || path.starts_with("extensions.") {
                return Err(mlua::Error::external(
                    "extension settings cannot be reset from Lua commands",
                ));
            }
            let value = reset_store
                .lock()
                .map_err(|e| mlua::Error::external(format!("settings lock poisoned: {e}")))?
                .reset_path_at(&path, &settings_path)
                .map_err(|e| mlua::Error::external(e.to_string()))?;
            lua.to_value(&value)
        })
        .map_err(crate::util::errstr)?;
    settings_api
        .set("reset", reset)
        .map_err(crate::util::errstr)?;
    bone.set("settings", settings_api)
        .map_err(crate::util::errstr)?;

    Ok(())
}

fn register_settings_page(
    lua: &Lua,
    declaration: Table,
    registry: &super::settings_registry::SharedSettingsRegistry,
) -> mlua::Result<()> {
    let mut page: super::settings_registry::SettingsPage =
        lua.from_value(Value::Table(declaration))?;
    let bone: Table = lua.globals().get("bone")?;
    page.owner = bone
        .get::<Option<String>>("_settings_owner")?
        .unwrap_or_else(|| "init.lua".into());
    registry
        .write()
        .map_err(|e| mlua::Error::external(format!("settings registry lock poisoned: {e}")))?
        .register(page)
        .map_err(mlua::Error::external)
}

fn setup_theme_api(
    lua: &Lua,
    bone: &Table,
    settings: Arc<Mutex<Settings>>,
    settings_path: &Path,
) -> Result<(), String> {
    let themes_dir = settings_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("lua/themes");
    let theme = lua.create_table().map_err(crate::util::errstr)?;

    let list_dir = themes_dir.clone();
    let list = lua
        .create_function(move |lua, ()| {
            let mut names = Vec::new();
            if let Ok(entries) = std::fs::read_dir(&list_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|ext| ext.to_str()) == Some("lua")
                        && let Some(name) = path.file_stem().and_then(|name| name.to_str())
                    {
                        names.push(name.to_string());
                    }
                }
            }
            names.sort();
            lua.create_sequence_from(names)
        })
        .map_err(crate::util::errstr)?;
    theme.set("list", list).map_err(crate::util::errstr)?;

    let load_dir = themes_dir.clone();
    let load_store = Arc::clone(&settings);
    let load_path = settings_path.to_path_buf();
    let load = lua
        .create_function(move |lua, name: String| {
            load_theme(lua, &load_dir, &load_store, &load_path, &name)
                .map_err(mlua::Error::external)
        })
        .map_err(crate::util::errstr)?;
    theme.set("load", load).map_err(crate::util::errstr)?;
    bone.set("theme", theme).map_err(crate::util::errstr)?;

    let selected = settings
        .lock()
        .map_err(|e| format!("settings lock poisoned: {e}"))?
        .resolved()
        .theme
        .name
        .clone();
    if let Some(name) = selected
        && let Err(error) = load_theme(lua, &themes_dir, &settings, settings_path, &name)
    {
        super::ctx::runtime_warn_once(format!(
            "bone-lua warn: could not reload theme '{name}': {error}"
        ));
    }
    Ok(())
}

fn load_theme(
    lua: &Lua,
    themes_dir: &Path,
    settings: &Arc<Mutex<Settings>>,
    settings_path: &Path,
    name: &str,
) -> Result<(), String> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("theme name must contain only ASCII letters, digits, '-' or '_'".into());
    }
    let path = themes_dir.join(format!("{name}.lua"));
    let source = std::fs::read_to_string(&path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let value = lua
        .load(&source)
        .set_name(path.to_string_lossy())
        .eval::<Value>()
        .map_err(|error| format!("failed to evaluate theme '{name}': {error}"))?;
    let mut resolved: crate::config::settings::ThemeSettings = lua
        .from_value(value)
        .map_err(|error| format!("invalid theme '{name}': {error}"))?;
    resolved.name = Some(name.to_string());
    settings
        .lock()
        .map_err(|e| format!("settings lock poisoned: {e}"))?
        .replace_theme_at(resolved, settings_path)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
#[path = "api_tests.rs"]
mod api_tests;

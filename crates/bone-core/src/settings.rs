//! `settings.json` in the config dir: the choices made in the app (which
//! provider and model to use, TUI preferences, plugins' own settings), as
//! opposed to `core.lua` and `tui.lua`, which are yours and never written.
//!
//! The core owns the file: clients change it with `settings/set` and
//! `settings/reset`, the core checks the change, writes the file whole (a
//! temporary file renamed over it) and tells every client
//! (`settings/changed`). Paths are dotted keys, e.g. `tui.tool_detail`,
//! `models.qwen`, `web_search.num_results`.
//!
//! Precedence: `core.lua` defines the providers and their default models;
//! `provider` and `models.<name>` here choose among them; `BONE_*`
//! environment variables override both for one run. TUI settings apply
//! before `tui.lua`, which runs last.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

pub fn path(config_dir: &Path) -> PathBuf {
    config_dir.join("settings.json")
}

/// The saved settings: an object, empty when there is no file. A file that
/// is not a JSON object is an error (it is never overwritten silently).
pub fn load(config_dir: &Path) -> Result<Value, String> {
    let path = path(config_dir);
    match std::fs::read_to_string(&path) {
        Ok(text) if text.trim().is_empty() => Ok(Value::Object(Map::new())),
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(v @ Value::Object(_)) => Ok(v),
            Ok(_) => Err(format!("{} is not a JSON object", path.display())),
            Err(e) => Err(format!("{}: {e}", path.display())),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Value::Object(Map::new())),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

/// Write the settings whole, atomically.
pub fn save(config_dir: &Path, settings: &Value) -> Result<(), String> {
    let path = path(config_dir);
    let tmp = config_dir.join(".settings.json.tmp");
    let text = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())? + "\n";
    std::fs::create_dir_all(config_dir)
        .and_then(|_| std::fs::write(&tmp, text))
        .and_then(|_| std::fs::rename(&tmp, &path))
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

fn keys(path: &str) -> Result<Vec<&str>, String> {
    let keys: Vec<&str> = path.split('.').collect();
    let ok = keys.iter().all(|k| {
        !k.is_empty()
            && k.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    });
    if ok {
        Ok(keys)
    } else {
        Err(format!(
            "bad setting path {path:?}: dotted names of letters, digits, _ and -"
        ))
    }
}

/// The value at `path`, if set.
pub fn get<'a>(settings: &'a Value, path: &str) -> Option<&'a Value> {
    let mut at = settings;
    for k in keys(path).ok()? {
        at = at.get(k)?;
    }
    Some(at)
}

/// Check a change: the path's shape, and the types of the keys the core
/// itself reads.
pub fn validate(path: &str, value: &Value) -> Result<(), String> {
    let keys = keys(path)?;
    let string_or_null = |what: &str| {
        if value.is_string() || value.is_null() {
            Ok(())
        } else {
            Err(format!("{what} must be a string"))
        }
    };
    match keys.as_slice() {
        ["provider"] => string_or_null("provider"),
        ["models", _] => string_or_null(path),
        ["models"] => Err("set one provider's model: models.<provider>".into()),
        ["providers"] => Err("set one provider: providers.<name>".into()),
        ["providers", _] => match value {
            Value::Null => Ok(()),
            Value::Object(p) => {
                let s = |k: &str| p.get(k).is_none_or(Value::is_string);
                if !(s("base_url") && s("model") && s("type") && s("api_key_env")) {
                    Err(format!(
                        "{path}: base_url, model, type and api_key_env are strings"
                    ))
                } else if p.get("model").is_none() {
                    Err(format!("{path}: a provider needs a model"))
                } else if p.get("base_url").is_none() && p.get("type").is_none() {
                    Err(format!(
                        "{path}: a provider needs a base_url (or a Lua provider type)"
                    ))
                } else if p.contains_key("api_key") {
                    Err(format!(
                        "{path}: keys go in secrets.json (secrets/set), not settings"
                    ))
                } else {
                    Ok(())
                }
            }
            _ => Err(format!("{path} is an object")),
        },
        ["providers", _, _] => Err("set a whole provider: providers.<name>".into()),
        _ => Ok(()),
    }
}

/// Set `path` to `value` (`null` removes it, and empty objects left behind).
pub fn set(settings: &mut Value, path: &str, value: Value) -> Result<(), String> {
    validate(path, &value)?;
    let keys = keys(path)?;
    if !settings.is_object() {
        *settings = Value::Object(Map::new());
    }
    fn put(at: &mut Value, keys: &[&str], value: Value) {
        let Value::Object(map) = at else { return };
        if keys.len() == 1 {
            if value.is_null() {
                map.remove(keys[0]);
            } else {
                map.insert(keys[0].to_owned(), value);
            }
            return;
        }
        let child = map
            .entry(keys[0].to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        if !child.is_object() {
            if value.is_null() {
                return;
            }
            *child = Value::Object(Map::new());
        }
        put(child, &keys[1..], value);
        if child.as_object().is_some_and(Map::is_empty) {
            map.remove(keys[0]);
        }
    }
    put(settings, &keys, value);
    Ok(())
}

/// `secrets.json` next to it: API keys entered in the app, readable only
/// by the owner, never sent back to clients. `{ "providers": { name: key } }`.
pub fn secrets_path(config_dir: &Path) -> PathBuf {
    config_dir.join("secrets.json")
}

pub fn load_secrets(config_dir: &Path) -> Result<Value, String> {
    match std::fs::read_to_string(secrets_path(config_dir)) {
        Ok(text) => serde_json::from_str::<Value>(&text)
            .ok()
            .filter(Value::is_object)
            .ok_or_else(|| {
                format!(
                    "{} is not a JSON object",
                    secrets_path(config_dir).display()
                )
            }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Value::Object(Map::new())),
        Err(e) => Err(format!(
            "cannot read {}: {e}",
            secrets_path(config_dir).display()
        )),
    }
}

/// Save a provider's key (None removes it); the file is mode 0600.
pub fn set_secret(
    config_dir: &Path,
    provider: &str,
    key: Option<&str>,
) -> Result<Vec<String>, String> {
    keys(provider)?;
    let mut secrets = load_secrets(config_dir)?;
    let map = secrets
        .as_object_mut()
        .expect("an object")
        .entry("providers")
        .or_insert_with(|| Value::Object(Map::new()));
    if let Value::Object(m) = map {
        match key {
            Some(k) => {
                m.insert(provider.to_owned(), Value::String(k.to_owned()));
            }
            None => {
                m.remove(provider);
            }
        }
    }
    let path = secrets_path(config_dir);
    let tmp = config_dir.join(".secrets.json.tmp");
    let text = serde_json::to_string_pretty(&secrets).map_err(|e| e.to_string())? + "\n";
    let write = || -> std::io::Result<()> {
        use std::io::Write;
        #[cfg(unix)]
        use std::os::unix::fs::OpenOptionsExt;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        opts.mode(0o600);
        let mut f = opts.open(&tmp)?;
        f.write_all(text.as_bytes())?;
        std::fs::rename(&tmp, &path)
    };
    write().map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(secret_names(&secrets))
}

/// Which providers have a saved key.
pub fn secret_names(secrets: &Value) -> Vec<String> {
    let mut names: Vec<String> = secrets
        .get("providers")
        .and_then(Value::as_object)
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    names.sort();
    names
}

/// Which configured provider to use: the saved choice when it names one,
/// else `core.lua`'s.
pub fn chosen_provider<'a>(
    settings: &'a Value,
    providers: &std::collections::HashMap<String, crate::config::ProviderConfig>,
    configured: Option<&'a str>,
) -> Option<&'a str> {
    get(settings, "provider")
        .and_then(Value::as_str)
        .filter(|name| providers.contains_key(*name))
        .or(configured)
}

/// The saved model for `provider`, if any.
pub fn chosen_model<'a>(settings: &'a Value, provider: &str) -> Option<&'a str> {
    settings.get("models")?.get(provider)?.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn set_get_reset_and_save() {
        let mut s = json!({});
        set(&mut s, "tui.tool_detail", json!("rows")).unwrap();
        set(&mut s, "models.qwen", json!("big")).unwrap();
        assert_eq!(get(&s, "tui.tool_detail"), Some(&json!("rows")));
        assert_eq!(chosen_model(&s, "qwen"), Some("big"));
        set(&mut s, "tui.tool_detail", Value::Null).unwrap();
        assert_eq!(s, json!({ "models": { "qwen": "big" } }), "empty tables go");
        assert!(set(&mut s, "provider", json!(3)).is_err());
        assert!(set(&mut s, "bad path!", json!(1)).is_err());
        assert!(set(&mut s, "models", json!({})).is_err());

        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path()).unwrap(), json!({}));
        save(dir.path(), &s).unwrap();
        assert_eq!(load(dir.path()).unwrap(), s);
        std::fs::write(path(dir.path()), "[1]").unwrap();
        assert!(load(dir.path()).is_err());
    }
}

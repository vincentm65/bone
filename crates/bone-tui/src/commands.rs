//! Slash commands: the registry behind `bone.cmd.create`, typed argument
//! parsing, and running a command line. Every command, `/help` included,
//! is Lua (`runtime/lua/bone/commands.lua`); so is the `/` menu
//! (`runtime/lua/bone/menu.lua`).

use serde_json::{Map, Value as JsonValue};

use crate::app::App;

#[derive(Debug, Clone)]
struct ArgSpec {
    name: String,
    kind: String,
    required: bool,
    default: Option<JsonValue>,
    variadic: bool,
}

fn integer_default(value: &JsonValue) -> Option<i64> {
    if let Some(value) = value.as_i64() {
        return Some(value);
    }
    let value = value.as_f64()?;
    if value.is_finite()
        && value.fract() == 0.0
        && value >= i64::MIN as f64
        && value < 9_223_372_036_854_775_808.0
    {
        Some(value as i64)
    } else {
        None
    }
}

fn typed_default(value: &JsonValue, kind: &str, name: &str) -> Result<JsonValue, String> {
    match kind {
        "string" | "str" if value.is_string() => Ok(value.clone()),
        "integer" | "int" => integer_default(value)
            .map(JsonValue::from)
            .ok_or_else(|| format!("default for {name} takes an integer")),
        "number" | "float" => {
            let value = value
                .as_f64()
                .filter(|value| value.is_finite())
                .ok_or_else(|| format!("default for {name} takes a finite number"))?;
            serde_json::Number::from_f64(value)
                .map(JsonValue::Number)
                .ok_or_else(|| format!("default for {name} takes a finite number"))
        }
        "boolean" | "bool" if value.is_boolean() => Ok(value.clone()),
        _ => Err(format!("default for {name} does not match type {kind}")),
    }
}

fn arg_specs(value: &JsonValue) -> Result<Vec<ArgSpec>, String> {
    fn one(name: String, value: Option<&JsonValue>) -> Result<ArgSpec, String> {
        if name.is_empty() {
            return Err("argument names cannot be empty".into());
        }
        let object = value.and_then(JsonValue::as_object);
        let kind = object
            .and_then(|o| o.get("type"))
            .and_then(JsonValue::as_str)
            .unwrap_or("string")
            .to_ascii_lowercase();
        if !matches!(
            kind.as_str(),
            "string" | "str" | "integer" | "int" | "number" | "float" | "boolean" | "bool"
        ) {
            return Err(format!("unknown argument type {kind:?} for {name}"));
        }
        let variadic = object
            .and_then(|o| o.get("variadic"))
            .and_then(JsonValue::as_bool)
            .unwrap_or(false);
        let default = match object.and_then(|o| o.get("default")) {
            Some(value) if variadic => {
                let values = value.as_array().ok_or_else(|| {
                    format!("default for variadic argument {name} must be a list")
                })?;
                Some(JsonValue::Array(
                    values
                        .iter()
                        .map(|value| typed_default(value, &kind, &name))
                        .collect::<Result<_, _>>()?,
                ))
            }
            Some(value) => Some(typed_default(value, &kind, &name)?),
            None => None,
        };
        Ok(ArgSpec {
            name,
            kind,
            required: object
                .and_then(|o| o.get("required"))
                .and_then(JsonValue::as_bool)
                .unwrap_or(false),
            default,
            variadic,
        })
    }

    let specs = match value {
        JsonValue::Array(items) => items
            .iter()
            .enumerate()
            .map(|(i, item)| match item {
                JsonValue::String(name) => one(name.clone(), None),
                JsonValue::Object(object) => {
                    let name = object
                        .get("name")
                        .and_then(JsonValue::as_str)
                        .ok_or_else(|| format!("argument {i} needs a name"))?;
                    one(name.to_owned(), Some(item))
                }
                _ => Err(format!("argument {i} must be a name or table")),
            })
            .collect::<Result<Vec<_>, _>>()?,
        JsonValue::Object(object) => object
            .iter()
            .map(|(name, spec)| {
                if spec.is_string() {
                    let object = serde_json::json!({ "type": spec });
                    one(name.clone(), Some(&object))
                } else {
                    one(name.clone(), Some(spec))
                }
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err("command args must be a list or table".into()),
    };
    for (index, spec) in specs.iter().enumerate() {
        if specs[..index]
            .iter()
            .any(|previous| previous.name == spec.name)
        {
            return Err(format!("duplicate argument: {}", spec.name));
        }
        if spec.variadic && index + 1 != specs.len() {
            return Err(format!("variadic argument {} must be last", spec.name));
        }
    }
    Ok(specs)
}

pub(crate) fn validate_argument_specs(value: &JsonValue) -> Result<(), String> {
    arg_specs(value).map(|_| ())
}

fn parse_argument(token: &str, kind: &str, name: &str) -> Result<JsonValue, String> {
    match kind {
        "string" | "str" => Ok(JsonValue::String(token.to_owned())),
        "integer" | "int" => token
            .parse::<i64>()
            .map(JsonValue::from)
            .map_err(|_| format!("argument {name} takes an integer")),
        "number" | "float" => {
            let value = token
                .parse::<f64>()
                .map_err(|_| format!("argument {name} takes a number"))?;
            serde_json::Number::from_f64(value)
                .map(JsonValue::Number)
                .ok_or_else(|| format!("argument {name} takes a finite number"))
        }
        "boolean" | "bool" => match token {
            "true" | "on" | "yes" | "1" => Ok(JsonValue::Bool(true)),
            "false" | "off" | "no" | "0" => Ok(JsonValue::Bool(false)),
            _ => Err(format!("argument {name} takes true or false")),
        },
        _ => unreachable!("validated in arg_specs"),
    }
}

fn structured_args(spec: Option<&JsonValue>, raw: &str) -> Result<Option<JsonValue>, String> {
    let Some(spec) = spec else { return Ok(None) };
    let specs = arg_specs(spec)?;
    let tokens: Vec<&str> = raw.split_whitespace().collect();
    let mut at = 0;
    let mut result = Map::new();
    for arg in &specs {
        if arg.variadic {
            let values = tokens[at..]
                .iter()
                .map(|token| parse_argument(token, &arg.kind, &arg.name))
                .collect::<Result<Vec<_>, _>>()?;
            if values.is_empty() {
                if let Some(default) = &arg.default {
                    result.insert(arg.name.clone(), default.clone());
                } else if arg.required {
                    return Err(format!("missing argument: {}", arg.name));
                } else {
                    result.insert(arg.name.clone(), JsonValue::Array(values));
                }
            } else {
                result.insert(arg.name.clone(), JsonValue::Array(values));
            }
            at = tokens.len();
            continue;
        }
        if let Some(token) = tokens.get(at) {
            result.insert(
                arg.name.clone(),
                parse_argument(token, &arg.kind, &arg.name)?,
            );
            at += 1;
        } else if let Some(default) = &arg.default {
            result.insert(arg.name.clone(), default.clone());
        } else if arg.required {
            return Err(format!("missing argument: {}", arg.name));
        } else {
            result.insert(arg.name.clone(), JsonValue::Null);
        }
    }
    if let Some(token) = tokens.get(at) {
        return Err(format!("unexpected argument: {token}"));
    }
    Ok(Some(JsonValue::Object(result)))
}

impl App {
    fn model_command(&mut self, args: &str) {
        let (args, session_id) = (args.to_owned(), self.current_session_id());
        self.request::<bone_proto::methods::ModelList>(
            bone_proto::methods::MaybeSession {
                session_id: session_id.clone(),
            },
            move |app, r| {
                let models = match r {
                    Ok(models) => models,
                    Err(e) => return app.error(format!("cannot list models: {e}")),
                };
                let Some(current) = models.into_iter().find(|model| model.current) else {
                    return app.error("no model provider is configured");
                };
                if args.is_empty() {
                    return app.info(format!("{} ({})", current.model, current.name));
                }
                let model = args.clone();
                app.request::<bone_proto::methods::SettingsSet>(
                    bone_proto::methods::SettingSet {
                        path: format!("providers.{}.model", current.name),
                        value: serde_json::Value::String(args),
                        session_id,
                    },
                    move |app, r| match r {
                        Ok(_) => app.info(format!("model: {} ({})", model, current.name)),
                        Err(e) => app.error(format!("cannot set model: {e}")),
                    },
                );
            },
        );
    }

    /// Run a command line (without the leading `/`).
    pub fn execute(&mut self, line: &str) {
        let line = line.trim().trim_start_matches('/');
        let (word, args) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let args = args.trim();
        if word == "model" {
            return self.model_command(args);
        }
        if let Some((name, cmd)) = self
            .user_commands
            .iter()
            .find(|(canonical, command)| {
                canonical.as_str() == word || command.aliases.iter().any(|alias| alias == word)
            })
            .map(|(name, command)| (name.clone(), command.clone()))
        {
            let raw_args = args.to_owned();
            let parsed = match structured_args(cmd.args.as_ref(), args) {
                Ok(parsed) => parsed,
                Err(e) => return self.error(e),
            };
            let argv = JsonValue::Array(
                args.split_whitespace()
                    .map(|arg| JsonValue::String(arg.to_owned()))
                    .collect(),
            );
            self.call_callback(cmd.callback, &format!("/{name}"), |lua| {
                let t = lua.create_table()?;
                t.set("args", raw_args)?;
                t.set("argv", bone_lua::to_lua(lua, &argv)?)?;
                t.set("command", name.as_str())?;
                if let Some(parsed) = parsed {
                    let value = bone_lua::to_lua(lua, &parsed)?;
                    t.set("arguments", value.clone())?;
                    // `parsed` is an explicit alias for plugins that prefer
                    // not to overload the word "arguments".
                    t.set("parsed", value)?;
                }
                Ok(mlua::Value::Table(t))
            });
            return;
        }
        self.error(format!("Unknown command /{word}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_argument_values_keep_raw_args() {
        let spec = serde_json::json!([
            { "name": "name", "required": true },
            { "name": "count", "type": "integer", "default": 2 },
            { "name": "enabled", "type": "boolean", "default": false },
        ]);
        let got = structured_args(Some(&spec), "bob 7 true").unwrap();
        assert_eq!(
            got,
            Some(serde_json::json!({
                "name": "bob",
                "count": 7,
                "enabled": true,
            }))
        );
        assert!(
            structured_args(Some(&spec), "")
                .unwrap_err()
                .contains("missing argument")
        );
        assert!(
            structured_args(Some(&spec), "bob nope")
                .unwrap_err()
                .contains("integer")
        );
        let variadic = serde_json::json!([
            { "name": "name", "required": true },
            { "name": "tags", "type": "string", "variadic": true },
        ]);
        assert_eq!(
            structured_args(Some(&variadic), "bob one two").unwrap(),
            Some(serde_json::json!({ "name": "bob", "tags": ["one", "two"] }))
        );
        assert_eq!(
            structured_args(Some(&variadic), "bob").unwrap(),
            Some(serde_json::json!({ "name": "bob", "tags": [] }))
        );
        assert!(
            structured_args(Some(&spec), "bob 7 true extra")
                .unwrap_err()
                .contains("unexpected argument")
        );
        assert!(
            validate_argument_specs(&serde_json::json!([
                { "name": "count", "type": "integer", "default": "bad" },
            ]))
            .unwrap_err()
            .contains("default")
        );
        assert!(
            validate_argument_specs(&serde_json::json!([
                { "name": "tag", "variadic": true },
                "other",
            ]))
            .unwrap_err()
            .contains("must be last")
        );
    }
}

//! Slash commands (`/new`, `/sessions`, `/set` ...), typed in the prompt.

use bone_proto::methods::{Empty, SessionList};
use serde_json::{Map, Value as JsonValue};

use crate::app::{App, Level};
use crate::keymap::{Action, Builtin};
use crate::options;
use crate::theme::StyleSpec;

pub struct Command {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub help: &'static str,
}

pub const COMMANDS: &[Command] = &[
    Command {
        name: "help",
        aliases: &["?"],
        help: "commands and keys; /help topic searches the docs",
    },
    Command {
        name: "health",
        aliases: &["checkhealth"],
        help: "check the setup: provider, terminal, clipboard, Lua",
    },
    Command {
        name: "new",
        aliases: &[],
        help: "start a new session",
    },
    Command {
        name: "sessions",
        aliases: &["resume"],
        help: "pick a session to open (ctrl+r)",
    },
    Command {
        name: "open",
        aliases: &[],
        help: "open a session by id or id prefix",
    },
    Command {
        name: "rename",
        aliases: &[],
        help: "give this session a title",
    },
    Command {
        name: "fork",
        aliases: &[],
        help: "copy this session to try something else; /fork N starts before turn N",
    },
    Command {
        name: "delete",
        aliases: &[],
        help: "delete this session (/delete yes)",
    },
    Command {
        name: "cancel",
        aliases: &[],
        help: "cancel the running turn",
    },
    Command {
        name: "quit",
        aliases: &["exit", "q"],
        help: "quit bone",
    },
    Command {
        name: "set",
        aliases: &[],
        help: "options: name, noname, name=value, name?",
    },
    Command {
        name: "colorscheme",
        aliases: &["theme"],
        help: "black (default), ansi, or your own",
    },
    Command {
        name: "highlight",
        aliases: &["hi"],
        help: "set a color: /hi Group fg=#rrggbb bg=... bold",
    },
    Command {
        name: "lua",
        aliases: &[],
        help: "run Lua; /lua =expr shows a value",
    },
    Command {
        name: "source",
        aliases: &[],
        help: "run a Lua file",
    },
    Command {
        name: "plugin",
        aliases: &["plugins"],
        help: "plugins: list, or load/unload/reload name",
    },
    Command {
        name: "project",
        aliases: &[],
        help: "this project's .bone/tui.lua: show, trust, untrust",
    },
    Command {
        name: "messages",
        aliases: &[],
        help: "recent messages and full Lua errors",
    },
];

impl App {
    /// `/rename title`, `/fork [turn]`, `/delete yes` on the session on
    /// screen.
    fn session_command(
        &mut self,
        name: &str,
        session_id: String,
        args: &str,
    ) -> Result<(), String> {
        use bone_proto::methods::{
            SessionDelete, SessionFork, SessionForkParams, SessionRef, SessionRename,
            SessionRenameParams,
        };
        match name {
            "rename" => {
                if args.is_empty() {
                    return Err("usage: /rename title".into());
                }
                let p = SessionRenameParams {
                    session_id,
                    title: args.to_owned(),
                };
                self.request::<SessionRename>(p, |app, r| match r {
                    Ok(info) => {
                        let title = info.title.clone().unwrap_or_default();
                        if let Some(buf) = app.chat_by_session(&info.session_id)
                            && let Some(c) = app.chat_mut(buf)
                        {
                            c.session = Some(info);
                        }
                        app.info(format!("renamed to {title}"));
                    }
                    Err(e) => app.error(format!("rename failed: {e}")),
                });
            }
            "fork" => {
                let before_turn = match args {
                    "" => None,
                    n => Some(n.parse::<u32>().map_err(|_| "usage: /fork [turn number]")?),
                };
                let p = SessionForkParams {
                    session_id,
                    before_turn,
                };
                self.request::<SessionFork>(p, move |app, r| match r {
                    Ok(info) => {
                        app.open_session(info.session_id);
                        app.info(match before_turn {
                            Some(n) => format!("forked from before turn {n}"),
                            None => "forked".to_owned(),
                        });
                    }
                    Err(e) => app.error(format!("fork failed: {e}")),
                });
            }
            _ => {
                if args != "yes" {
                    self.info("Delete this session and its file? /delete yes");
                    return Ok(());
                }
                self.request::<SessionDelete>(SessionRef { session_id }, |app, r| {
                    if let Err(e) = r {
                        app.error(format!("delete failed: {e}"));
                    }
                });
            }
        }
        Ok(())
    }
}

/// The command a name or alias refers to.
pub fn resolve(word: &str) -> Option<&'static str> {
    COMMANDS
        .iter()
        .find(|c| c.name == word || c.aliases.contains(&word))
        .map(|c| c.name)
}

const KEYS_HELP: &str = "\
Keys: enter send · alt+enter newline · ctrl+c cancel / clear / quit · ctrl+r sessions
      pageup/pagedown scroll · ctrl+home/ctrl+end top/bottom · up/down history · tab complete";

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

fn command_names(name: &str, aliases: &[String]) -> String {
    if aliases.is_empty() {
        format!("/{name}")
    } else {
        format!("/{name} (aliases: {})", aliases.join(", "))
    }
}

impl App {
    /// Run a command line (without the leading `/`).
    pub fn execute(&mut self, line: &str) {
        let line = line.trim().trim_start_matches('/');
        let (word, args) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let args = args.trim();
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
        let Some(name) = resolve(word) else {
            return self.error(format!("Unknown command /{word}"));
        };
        if let Err(e) = self.command(name, args) {
            self.error(e);
        }
    }

    fn command(&mut self, name: &str, args: &str) -> Result<(), String> {
        match name {
            "help" if !args.is_empty() => self.call_ui("help", Some(args)),
            "health" => self.call_ui("health", None),
            "plugin" => self.plugin_command(args)?,
            "project" => self.project_command(args)?,
            "help" => {
                let mut rows: Vec<String> = COMMANDS
                    .iter()
                    .map(|c| {
                        let aliases = if c.aliases.is_empty() {
                            String::new()
                        } else {
                            format!(" (aliases: {})", c.aliases.join(", "))
                        };
                        format!("/{:<12} {}{}", c.name, c.help, aliases)
                    })
                    .collect();
                let mut user: Vec<String> = self
                    .user_commands
                    .iter()
                    .map(|(n, c)| format!("{:<28} {}", command_names(n, &c.aliases), c.desc))
                    .collect();
                user.sort();
                rows.extend(user);
                rows.push(String::new());
                rows.push(KEYS_HELP.to_owned());
                rows.push(
                    "Docs: /help topic (e.g. /help hooks, /help windows, /help keys)".to_owned(),
                );
                self.info(rows.join("\n"));
            }
            "new" => self.new_session(),
            "sessions" => self.open_picker(),
            "rename" | "fork" | "delete" => {
                let session_id = self.chats[self.current]
                    .session_id()
                    .map(str::to_owned)
                    .ok_or("this session has no messages yet")?;
                self.session_command(name, session_id, args)?;
            }
            "open" => {
                if args.is_empty() {
                    return Err("usage: /open {session id or prefix}".into());
                }
                let prefix = args.to_owned();
                self.request::<SessionList>(Empty {}, move |app, r| {
                    let list = match r {
                        Ok(l) => l,
                        Err(e) => return app.error(format!("cannot list sessions: {e}")),
                    };
                    let hits: Vec<_> = list
                        .into_iter()
                        .filter(|s| s.session_id.starts_with(&prefix))
                        .collect();
                    match hits.as_slice() {
                        [one] => app.open_session(one.session_id.clone()),
                        [] => app.error(format!("No session matches {prefix}")),
                        _ => app.error(format!("{} sessions match {prefix}", hits.len())),
                    }
                });
            }
            "cancel" => self.run(Action::Builtin(Builtin::Interrupt)),
            "quit" => self.quit = Some(None),
            "set" => {
                if args.is_empty() {
                    let mut all: Vec<String> = options::NAMES
                        .iter()
                        .map(|n| format!("{n}={}", self.options.get(n).unwrap()))
                        .collect();
                    let mut dynamic: Vec<String> = self
                        .dynamic_options
                        .iter()
                        .map(|(name, option)| format!("{name}={}", option.value))
                        .collect();
                    dynamic.sort();
                    all.extend(dynamic);
                    self.info(all.join("  "));
                }
                let mut shown = Vec::new();
                for arg in args.split_whitespace() {
                    if let Some(result) = crate::lua::apply_dynamic_option(self, arg) {
                        if let Some(s) = result? {
                            shown.push(s);
                        }
                    } else if let Some(s) = self.options.apply(arg)? {
                        shown.push(s);
                    }
                }
                self.opts_rev += 1;
                if !shown.is_empty() {
                    self.info(shown.join("  "));
                }
            }
            "colorscheme" => {
                if args.is_empty() {
                    let name = self
                        .colors_name
                        .clone()
                        .unwrap_or_else(|| "(built-in)".into());
                    self.info(name);
                } else {
                    self.colorscheme(args)?;
                }
            }
            "highlight" => {
                let mut words = args.split_whitespace();
                let Some(group) = words.next() else {
                    let names = self.theme.names().join(" ");
                    self.info(names);
                    return Ok(());
                };
                let attrs: Vec<&str> = words.collect();
                if attrs.is_empty() {
                    let s = self.theme.hl(group);
                    self.info(format!("{group} {s:?}"));
                    return Ok(());
                }
                let mut spec = StyleSpec::default();
                for a in attrs {
                    match a.split_once('=') {
                        Some(("fg", v)) => spec.fg = Some(v.into()),
                        Some(("bg", v)) => spec.bg = Some(v.into()),
                        Some(("link", v)) => spec.link = Some(v.into()),
                        None if a == "bold" => spec.bold = true,
                        None if a == "italic" => spec.italic = true,
                        None if a == "underline" => spec.underline = true,
                        None if a == "reverse" => spec.reverse = true,
                        None if a == "dim" => spec.dim = true,
                        _ => return Err(format!("bad highlight attribute {a:?}")),
                    }
                }
                let style = spec.to_style(&self.theme)?;
                self.theme.set(group, style);
                self.opts_rev += 1;
            }
            "lua" => self.exec_lua(args),
            "source" => {
                if args.is_empty() {
                    return Err("usage: /source {file}".into());
                }
                self.source(args);
            }
            "messages" => {
                let start = self.log.len().saturating_sub(20);
                let text = self.log[start..].join("\n");
                // Not via info(): showing the log must not grow it.
                self.message = (!text.is_empty()).then_some((text, Level::Info));
            }
            _ => unreachable!("unhandled command {name}"),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_aliases() {
        assert_eq!(resolve("q"), Some("quit"));
        assert_eq!(resolve("exit"), Some("quit"));
        assert_eq!(resolve("resume"), Some("sessions"));
        assert_eq!(resolve("hi"), Some("highlight"));
        assert_eq!(resolve("ses"), None);
    }

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

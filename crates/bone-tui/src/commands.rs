//! Slash commands: running a command line against the registry behind
//! `bone.cmd.create` (arguments are parsed in `bone-ui`). Every command,
//! `/help` included, is Lua (`runtime/lua/bone/commands.lua`); so is the
//! `/` menu (`runtime/lua/bone/menu.lua`).

use serde_json::Value as JsonValue;

use bone_ui::commands::structured_args;

use crate::app::App;

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

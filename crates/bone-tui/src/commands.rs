//! Slash commands (`/new`, `/sessions`, `/set` ...), typed in the prompt.

use bone_proto::methods::{Empty, SessionList};

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
        name: "messages",
        aliases: &[],
        help: "recent messages and full Lua errors",
    },
];

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

impl App {
    /// Run a command line (without the leading `/`).
    pub fn execute(&mut self, line: &str) {
        let line = line.trim().trim_start_matches('/');
        let (word, args) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let args = args.trim();
        if let Some(cmd) = self.user_commands.get(word) {
            let (cb, args, name) = (cmd.callback, args.to_owned(), word.to_owned());
            self.call_callback(cb, &format!("/{name}"), |lua| {
                let t = lua.create_table()?;
                t.set("args", args)?;
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
            "help" => {
                let mut rows: Vec<String> = COMMANDS
                    .iter()
                    .map(|c| format!("/{:<12} {}", c.name, c.help))
                    .collect();
                let mut user: Vec<String> = self
                    .user_commands
                    .iter()
                    .map(|(n, c)| format!("/{n:<12} {}", c.desc))
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
                    let all: Vec<String> = options::NAMES
                        .iter()
                        .map(|n| format!("{n}={}", self.options.get(n).unwrap()))
                        .collect();
                    self.info(all.join("  "));
                }
                let mut shown = Vec::new();
                for arg in args.split_whitespace() {
                    if let Some(s) = self.options.apply(arg)? {
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
}

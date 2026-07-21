//! Built-in slash-command metadata.
//!
//! Single source of truth for the built-in command names. These are protected:
//! a Lua command cannot override them, and the config page builder skips them
//! when listing toggleable commands — toggling one would be a no-op, since the
//! dispatch bypass and the `is_protected_builtin` guard run before the
//! deny-list branch ever sees them.

/// Built-in slash commands as (name, description) pairs. The description is
/// presentation text surfaced by `/help` and autocomplete; the name is what
/// matters for protection.
pub const BUILTINS: &[(&str, &str)] = &[
    ("catalog", "browse & install optional tools and commands"),
    ("clear", "clear chat history"),
    ("config", "change application settings"),
    ("edit", "open system editor for input"),
    ("e", "open system editor for input"),
    ("exit", "exit bone"),
    ("help", "show this message"),
    ("model", "set or show model (/model <name>)"),
    ("new", "clear chat history (alias for /clear)"),
    ("provider", "pick or switch provider (/provider <name>)"),
    ("quit", "exit bone"),
    ("setup", "re-run the onboarding setup wizard"),
    ("stats", "open full-screen token stats dashboard"),
    ("tools", "enable or disable tools, /tools reload to rescan"),
    ("update", "check and apply bone updates"),
];

/// Whether `cmd` names a built-in slash command. Built-ins cannot be overridden
/// by a Lua command of the same name and are never shown as toggleable.
pub fn is_protected_builtin(cmd: &str) -> bool {
    BUILTINS.iter().any(|(name, _)| *name == cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agents_is_dispatched_as_a_lua_command() {
        assert!(!is_protected_builtin("agents"));
    }
}

//! Slash-command metadata (built-ins, /help text). Dispatch lives in
//! `App::handle_command`, which routes turns/lifecycle through the daemon.
//!
//! The canonical built-in list and `is_protected_builtin` live in
//! [`bone_core::commands`] (shared with the config page builder, which skips
//! protected built-ins so they never appear as no-op toggles). Re-exported here
//! so existing `commands::BUILTINS` / `commands::is_protected_builtin` call
//! sites keep resolving.

pub use bone_core::commands::{BUILTINS, is_protected_builtin};

pub fn merge_commands(advertised: &[(String, String)]) -> Vec<(String, String)> {
    let mut commands: std::collections::BTreeMap<String, String> = BUILTINS
        .iter()
        .map(|(name, description)| ((*name).into(), (*description).into()))
        .collect();
    for (name, description) in advertised {
        commands
            .entry(name.clone())
            .or_insert_with(|| description.clone());
    }
    commands.into_iter().collect()
}

pub fn help(advertised_commands: &[(String, String)]) -> String {
    let bold = "\x1b[1m";
    let reset = "\x1b[0m";
    let commands = merge_commands(advertised_commands);
    let max_name = commands
        .iter()
        .map(|(name, _)| name.len())
        .max()
        .unwrap_or(0)
        .max(10);
    let mut lines = vec![format!("{bold}Commands{reset}")];
    for (name, description) in commands {
        lines.push(format!("  /{name:<max_name$} — {description}"));
    }
    lines.push("  :           — run a shell command inline (: <command>)".to_string());
    lines.push(String::new());
    lines.push(format!("{bold}Input shortcuts{reset}"));
    lines.push("  Ctrl+A       — move cursor to start of line".to_string());
    lines.push("  Ctrl+E       — move cursor to end of line".to_string());
    lines.push("  Alt+Left     — move cursor one word back".to_string());
    lines.push("  Alt+Right    — move cursor one word forward".to_string());
    lines.push("  Ctrl+W       — delete word backward".to_string());
    lines.push("  Ctrl+U       — clear the entire input".to_string());
    lines.push("  Ctrl+K       — clear line after cursor".to_string());
    lines.push("  Ctrl+X       — open system editor".to_string());
    lines.push("  Ctrl+D       — clear message queue".to_string());
    lines.push("  Ctrl+C       — cancel streaming (double-tap to quit)".to_string());
    lines.push("  Esc          — clear input buffer".to_string());
    lines.push(String::new());
    lines.push(format!("{bold}Pane navigation{reset}"));
    lines.push("  Ctrl+T       — toggle pane visibility".to_string());
    lines.push("  Tab          — cycle active pane (when panes visible)".to_string());
    lines.push(
        "  Shift+Tab    — cycle approval mode (auto-accepts pending tools in Danger)".to_string(),
    );
    lines.push("  PageUp/Down  — scroll active pane".to_string());
    lines.push("  Ctrl+Up/Down — scroll active pane by one line".to_string());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertised_agents_appears_once_in_help_and_discovery() {
        let advertised = vec![
            ("agents".into(), "manage named sub-agents".into()),
            ("config".into(), "Lua duplicate".into()),
        ];
        let commands = merge_commands(&advertised);
        assert_eq!(
            commands.iter().filter(|(name, _)| name == "agents").count(),
            1
        );
        assert_eq!(
            commands.iter().filter(|(name, _)| name == "config").count(),
            1
        );
        assert!(help(&advertised).contains("/agents"));
    }
}

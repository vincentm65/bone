//! Slash-command metadata (built-ins, /help text). Dispatch lives in
//! `App::handle_command`, which routes turns/lifecycle through the daemon.
//!
//! The canonical built-in list and `is_protected_builtin` live in
//! [`bone_core::commands`] (shared with the config page builder, which skips
//! protected built-ins so they never appear as no-op toggles). Re-exported here
//! so existing `commands::BUILTINS` / `commands::is_protected_builtin` call
//! sites keep resolving.

pub use bone_core::commands::{BUILTINS, is_protected_builtin};

pub fn help(downloaded_commands: &[(String, String)]) -> String {
    let bold = "\x1b[1m";
    let reset = "\x1b[0m";
    let mut lines: Vec<String> = BUILTINS
        .iter()
        .map(|(name, desc)| format!("  /{name:10} — {desc}"))
        .collect();
    lines.insert(0, format!("{bold}Commands{reset}"));
    lines.push("  :           — run a shell command inline (: <command>)".to_string());
    if !downloaded_commands.is_empty() {
        lines.push(String::new());
        lines.push(format!("{bold}Downloaded commands{reset}"));
        let max_name = downloaded_commands
            .iter()
            .map(|(n, _)| n.len())
            .max()
            .unwrap_or(0)
            .max(10);
        for (name, desc) in downloaded_commands {
            lines.push(format!("  /{name:<max_name$} — {desc}"));
        }
    }
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
    lines.push("  Shift+Tab    — cycle approval mode (or pane when visible)".to_string());
    lines.push("  PageUp/Down  — scroll active pane".to_string());
    lines.push("  Ctrl+Up/Down — scroll active pane by one line".to_string());
    lines.join("\n")
}

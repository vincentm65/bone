//! Slash-command metadata (built-ins, /help text). Dispatch lives in
//! `App::handle_command`, which routes turns/lifecycle through the daemon.

/// Built-in slash commands as (name, description) pairs.
/// Single source of truth for autocomplete, /help, and override protection.
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
];

/// Built-in slash commands that Lua commands cannot override.
pub fn is_protected_builtin(cmd: &str) -> bool {
    BUILTINS.iter().any(|(name, _)| *name == cmd)
}

pub fn help(lua_commands: &[(String, String)]) -> String {
    let bold = "\x1b[1m";
    let reset = "\x1b[0m";
    let mut lines: Vec<String> = BUILTINS
        .iter()
        .map(|(name, desc)| format!("  /{name:10} — {desc}"))
        .collect();
    lines.insert(0, format!("{bold}Commands{reset}"));
    lines.push("  :           — run a shell command inline (: <command>)".to_string());
    if !lua_commands.is_empty() {
        lines.push(String::new());
        lines.push(format!("{bold}Lua commands{reset}"));
        let max_name = lua_commands
            .iter()
            .map(|(n, _)| n.len())
            .max()
            .unwrap_or(0)
            .max(10);
        for (name, desc) in lua_commands {
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

use crate::config;
use crate::llm::{LlmProvider, providers};

/// Result of executing a slash command.
pub enum CommandResult {
    /// Continue normally. `reply` is printed as a system message.
    Continue { reply: String },
    /// Quit the application.
    Quit,
    /// Open the system editor and return the contents to the input buffer.
    OpenEditor,
}

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

/// Dispatch a slash command. Returns a reply string or a quit signal.
#[allow(clippy::too_many_arguments)]
pub async fn handle(
    cmd: &str,
    arg: &str,
    llm: &mut std::sync::Arc<dyn LlmProvider>,
    provider_label: &mut String,
    model_label: &mut String,
    custom: &mut config::custom::CustomConfigs,
    lua_commands: &[(String, String)],
) -> std::io::Result<CommandResult> {
    let reply = match cmd {
        "help" => help(lua_commands),

        "model" => model_switch(arg, llm, provider_label, model_label, custom),
        "provider" => provider_switch(arg, llm, provider_label, model_label, custom).await,
        "quit" | "exit" => {
            return Ok(CommandResult::Quit);
        }
        "edit" | "e" => {
            return Ok(CommandResult::OpenEditor);
        }
        _ => format!("Unknown command: /{cmd}. Type /help for available commands."),
    };

    Ok(CommandResult::Continue { reply })
}

fn help(lua_commands: &[(String, String)]) -> String {
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
fn model_switch(
    arg: &str,
    llm: &mut std::sync::Arc<dyn LlmProvider>,
    provider_label: &mut String,
    model_label: &mut String,
    custom: &mut config::custom::CustomConfigs,
) -> String {
    if arg.is_empty() {
        return format!("{} ({})", model_label, provider_label);
    }

    let id = llm.id().to_string();
    if let Some(entry) = custom.get_provider_entry("providers", &id) {
        let mut entry = entry;
        entry.model = arg.to_string();
        custom.set_provider_entry("providers", &id, &entry);
    }
    // The provider is shared (Arc) with the runtime Driver. Model switches
    // happen between turns, when the App holds the only reference, so
    // `get_mut` succeeds; if it's momentarily shared, recreate from config.
    if let Some(p) = std::sync::Arc::get_mut(llm) {
        p.set_model(arg.to_string());
    } else if let Ok(fresh) =
        providers::create_provider_with_config(&id, &custom.derive_providers_config())
    {
        *llm = std::sync::Arc::from(fresh);
    }
    *model_label = arg.to_string();
    format!("Switched to {} ({})", arg, provider_label)
}
async fn provider_switch(
    arg: &str,
    llm: &mut std::sync::Arc<dyn LlmProvider>,
    provider_label: &mut String,
    model_label: &mut String,
    custom: &mut config::custom::CustomConfigs,
) -> String {
    if arg.is_empty() {
        let mut lines = vec![format!("Current: {} ({})", model_label, provider_label)];
        lines.push(String::new());
        let providers_config = custom.derive_providers_config();
        if providers_config.providers.is_empty() {
            lines.push(
                "No providers configured. Edit ~/.bone-rust/config/providers.yaml".to_string(),
            );
        } else {
            lines.push("Available:".to_string());
            for (id, entry) in &providers_config.providers {
                let marker = if id == llm.id() { " *" } else { "" };
                lines.push(format!(
                    "  {} — {} ({}){}",
                    id, entry.label, entry.model, marker
                ));
            }
        }
        lines.join("\n")
    } else {
        match providers::create_provider_with_config(arg, &custom.derive_providers_config()) {
            Ok(new_provider) => match new_provider.validate().await {
                Ok(()) => {
                    let label = format!("{} ({})", new_provider.name(), new_provider.id());
                    let model = new_provider.model().to_string();
                    *llm = std::sync::Arc::from(new_provider);
                    *provider_label = label.clone();
                    *model_label = model.clone();
                    custom.set_last_provider(arg);
                    format!("Switched to {} ({})", model, label)
                }
                Err(err) => format!("Provider validation failed: {err}"),
            },
            Err(err) => err.to_string(),
        }
    }
}

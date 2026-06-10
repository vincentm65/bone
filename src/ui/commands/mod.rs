use crate::chat::Message;
use crate::config;
use crate::config::ProvidersConfig;
use crate::llm::{ChatMessage, ChatRole, LlmProvider, TokenStats, providers};
use crate::ui::render::{BoneTerminal, Renderer};

/// Result of executing a slash command.
pub enum CommandResult {
    /// Continue normally. `reply` is printed as a system message.
    Continue { reply: String },
    /// Quit the application.
    Quit,
    /// Open the system editor and return the contents to the input buffer.
    OpenEditor,
}

/// Built-in slash commands that Lua commands cannot override.
pub fn is_protected_builtin(cmd: &str) -> bool {
    matches!(
        cmd,
        "help"
            | "quit"
            | "exit"
            | "new"
            | "clear"
            | "compact"
            | "model"
            | "provider"
            | "config"
            | "tools"
            | "edit"
            | "e"
            | "stats"
            | "usage"
    )
}

/// Dispatch a slash command. Returns a reply string or a quit signal.
#[allow(clippy::too_many_arguments)]
pub async fn handle(
    cmd: &str,
    arg: &str,
    messages: &mut Vec<Message>,
    transcript: &mut Vec<ChatMessage>,
    token_stats: &mut TokenStats,
    renderer: &mut Renderer,
    term: &mut BoneTerminal,
    llm: &mut Box<dyn LlmProvider>,
    provider_label: &mut String,
    model_label: &mut String,
    providers_config: &mut ProvidersConfig,
) -> std::io::Result<CommandResult> {
    let reply = match cmd {
        "help" => help(),
        "clear" | "new" => clear(
            messages,
            transcript,
            token_stats,
            renderer,
            term,
            provider_label,
            model_label,
        )?,
        "model" => model_switch(arg, llm, provider_label, model_label, providers_config),
        "provider" => {
            provider_switch(arg, llm, provider_label, model_label, providers_config).await
        }
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

#[allow(clippy::too_many_arguments)]
fn clear(
    messages: &mut Vec<Message>,
    transcript: &mut Vec<ChatMessage>,
    token_stats: &mut TokenStats,
    renderer: &mut Renderer,
    term: &mut BoneTerminal,
    provider_label: &str,
    model_label: &str,
) -> std::io::Result<String> {
    renderer.flush_new_to_scrollback(messages, term)?;
    crossterm::execute!(
        term.backend_mut(),
        crossterm::terminal::Clear(crossterm::terminal::ClearType::Purge)
    )?;
    if let Some(msg) = messages.first().cloned() {
        if msg.role == ChatRole::System {
            *messages = vec![msg];
        } else {
            messages.clear();
        }
    }
    transcript.clear();
    *token_stats = TokenStats::new();
    renderer.scrollback_cursor = messages.len();
    renderer.render_banner(term, provider_label, model_label)?;
    Ok("Chat cleared.".to_string())
}
fn help() -> String {
    let bold = "\x1b[1m";
    let reset = "\x1b[0m";
    vec![
        format!("{bold}Commands{reset}"),
        "  /clear      — clear chat history".to_string(),
        "  /config     — change application settings".to_string(),
        "  /edit, /e   — open system editor for input".to_string(),
        "  /help       — show this message".to_string(),
        "  /model      — set or show model (/model <name>)".to_string(),
        "  /new        — clear chat history (alias for /clear)".to_string(),
        "  /provider   — pick or switch provider (/provider <name>)".to_string(),
        "  /stats      — open full-screen token stats dashboard".to_string(),
        "  /tools      — enable or disable tools, /tools reload to rescan".to_string(),
        "  /usage      — show token usage for current conversation".to_string(),
        "  /quit, /exit— exit bone".to_string(),
        "  :           — run a shell command inline (: <command>)".to_string(),
        String::new(),
        format!("{bold}Input shortcuts{reset}"),
        "  Ctrl+A       — move cursor to start of line".to_string(),
        "  Ctrl+E       — move cursor to end of line".to_string(),
        "  Ctrl+W       — delete word backward".to_string(),
        "  Ctrl+U       — clear line before cursor".to_string(),
        "  Ctrl+K       — clear line after cursor".to_string(),
        "  Ctrl+X       — open system editor".to_string(),
        "  Ctrl+D       — clear message queue".to_string(),
        "  Ctrl+C       — cancel streaming (double-tap to quit)".to_string(),
        "  Esc          — clear input buffer".to_string(),
        String::new(),
        format!("{bold}Pane navigation{reset}"),
        "  Ctrl+T       — toggle pane visibility".to_string(),
        "  Tab          — cycle active pane (when panes visible)".to_string(),
        "  Shift+Tab    — cycle approval mode".to_string(),
        "  PageUp/Down  — scroll active pane".to_string(),
        "  Ctrl+Up/Down — scroll active pane by one line".to_string(),
    ]
    .join("\n")
}
fn model_switch(
    arg: &str,
    llm: &mut Box<dyn LlmProvider>,
    provider_label: &mut String,
    model_label: &mut String,
    providers_config: &mut ProvidersConfig,
) -> String {
    if arg.is_empty() {
        return format!("{} ({})", model_label, provider_label);
    }

    let entry = providers_config.providers.get_mut(llm.id()).unwrap();
    entry.model = arg.to_string();
    config::providers_config::save_providers(providers_config);
    llm.set_model(arg.to_string());
    *model_label = arg.to_string();
    format!("Switched to {} ({})", arg, provider_label)
}
async fn provider_switch(
    arg: &str,
    llm: &mut Box<dyn LlmProvider>,
    provider_label: &mut String,
    model_label: &mut String,
    providers_config: &mut ProvidersConfig,
) -> String {
    if arg.is_empty() {
        let mut lines = vec![format!("Current: {} ({})", model_label, provider_label)];
        lines.push(String::new());
        if providers_config.providers.is_empty() {
            lines.push(
                "No providers configured. Create ~/.bone-rust/config/providers.yaml".to_string(),
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
        match providers::create_provider_with_config(arg, providers_config) {
            Ok(new_provider) => match new_provider.validate().await {
                Ok(()) => {
                    let label = format!("{} ({})", new_provider.name(), new_provider.id());
                    let model = new_provider.model().to_string();
                    *llm = new_provider;
                    *provider_label = label.clone();
                    *model_label = model.clone();
                    providers_config.last_provider = arg.to_string();
                    config::providers_config::save_providers(providers_config);
                    format!("Switched to {} ({})", model, label)
                }
                Err(err) => format!("Provider validation failed: {err}"),
            },
            Err(err) => err.to_string(),
        }
    }
}

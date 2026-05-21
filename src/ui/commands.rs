use crate::chat::Context;
use crate::config::ProvidersConfig;
use crate::llm::{ChatRole, LlmProvider};
use crate::llm::providers;
use super::input::Message;
use super::renderer::{BoneTerminal, Renderer};

/// Result of executing a slash command.
pub enum CommandResult {
    /// Continue normally. `reply` is printed as a system message.
    Continue { reply: String },
    /// Quit the application.
    Quit,
}

/// Dispatch a slash command. Returns a reply string or a quit signal.
#[allow(clippy::too_many_arguments)] // Will become a CommandContext struct when commands grow
pub async fn handle(
    cmd: &str,
    arg: &str,
    messages: &mut Vec<Message>,
    renderer: &mut Renderer,
    term: &mut BoneTerminal,
    context: &Context,
    llm: &mut Box<dyn LlmProvider>,
    provider_label: &mut String,
    model_label: &mut String,
    providers_config: &ProvidersConfig,
) -> std::io::Result<CommandResult> {
    let reply = match cmd {
        "/help" => [
            "/clear     — clear chat history",
            "/compact   — show context usage",
            "/help      — show this message",
            "/model     — show current model",
            "/provider  — show or switch provider (/provider <name>)",
            "/quit      — exit bone",
        ].join("\n"),

        "/clear" => {
            renderer.flush_new_to_scrollback(messages, term)?;
            let size = term.size().map(|s| s.height).unwrap_or(50);
            term.insert_before(size, |_buf| {})?;

            if let Some(msg) = messages.first().cloned() {
                if msg.role == ChatRole::System {
                    *messages = vec![msg];
                } else {
                    messages.clear();
                }
            }
            renderer.scrollback_cursor = messages.len();
            renderer.render_banner(term, provider_label, model_label)?;
            "Chat cleared.".to_string()
        }

        "/compact" => {
            let budget = context.budget();
            let used: usize = messages.iter().map(|m| Context::estimate_tokens(&m.content)).sum();
            let pct = used * 100 / budget.max(1);
            format!("Context: {used}/{budget} tokens ({pct}%)")
        }

        "/model" => format!("{} ({})", model_label, provider_label),

        "/provider" => {
            if arg.is_empty() {
                let mut lines = vec![format!("Current: {} ({})", model_label, provider_label)];
                lines.push(String::new());
                if providers_config.providers.is_empty() {
                    lines.push("No providers configured. Create ~/.bone-rust/providers.yaml".to_string());
                } else {
                    lines.push("Available:".to_string());
                    for (id, entry) in &providers_config.providers {
                        let marker = if id == llm.id() { " *" } else { "" };
                        lines.push(format!("  {} — {} ({}){}", id, entry.label, entry.model, marker));
                    }
                }
                lines.join("\n")
            } else {
                match providers::create_provider_with_config(arg, providers_config) {
                    Ok(new_provider) => {
                        match new_provider.validate().await {
                            Ok(()) => {
                                let label = format!("{} ({})", new_provider.name(), new_provider.id());
                                let model = new_provider.model().to_string();
                                *llm = new_provider;
                                *provider_label = label.clone();
                                *model_label = model.clone();
                                format!("Switched to {} ({})", model, label)
                            }
                            Err(err) => format!("Provider validation failed: {err}")
                        }
                    }
                    Err(err) => err.to_string()
                }
            }
        }

        "/quit" | "/exit" => {
            return Ok(CommandResult::Quit);
        }

        _ => format!("Unknown command: {cmd}. Type /help for available commands."),
    };

    Ok(CommandResult::Continue { reply })
}

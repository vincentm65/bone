mod clear;
mod context;
mod help;
mod model;
mod provider_switch;
mod quit;

use crate::chat::Message;
use crate::config::ProvidersConfig;
use crate::llm::{ChatMessage, LlmProvider, TokenStats};
use crate::ui::render::BoneTerminal;
use crate::ui::render::Renderer;

/// Result of executing a slash command.
pub enum CommandResult {
    /// Continue normally. `reply` is printed as a system message.
    Continue { reply: String },
    /// Quit the application.
    Quit,
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
        "/help" => help::run(),
        "/clear" | "/new" => clear::run(
            messages,
            transcript,
            token_stats,
            renderer,
            term,
            provider_label,
            model_label,
        )?,
        "/context" => context::run(transcript),
        "/model" => model::run(provider_label, model_label),
        "/provider" => {
            provider_switch::run(arg, llm, provider_label, model_label, providers_config).await
        }
        "/quit" | "/exit" => {
            return Ok(CommandResult::Quit);
        }
        _ => format!("Unknown command: {cmd}. Type /help for available commands."),
    };

    Ok(CommandResult::Continue { reply })
}

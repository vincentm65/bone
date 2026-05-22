use crate::chat::Message;
use crate::llm::{ChatMessage, ChatRole, TokenStats};
use crate::ui::render::{BoneTerminal, Renderer};

#[allow(clippy::too_many_arguments)]
pub fn run(
    messages: &mut Vec<Message>,
    transcript: &mut Vec<ChatMessage>,
    token_stats: &mut TokenStats,
    renderer: &mut Renderer,
    term: &mut BoneTerminal,
    provider_label: &str,
    model_label: &str,
) -> std::io::Result<String> {
    renderer.flush_new_to_scrollback(messages, term)?;

    // Use crossterm's ClearType::Purge to wipe scrollback + visible screen
    // without disturbing the inline viewport position. This is safer than
    // term.clear() which may invalidate the viewport's internal tracking.
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

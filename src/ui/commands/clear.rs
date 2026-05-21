use crate::chat::Message;
use crate::llm::ChatRole;
use crate::ui::render::{BoneTerminal, Renderer};

pub fn run(
    messages: &mut Vec<Message>,
    renderer: &mut Renderer,
    term: &mut BoneTerminal,
    provider_label: &str,
    model_label: &str,
) -> std::io::Result<String> {
    renderer.flush_new_to_scrollback(messages, term)?;
    term.clear()?;

    if let Some(msg) = messages.first().cloned() {
        if msg.role == ChatRole::System {
            *messages = vec![msg];
        } else {
            messages.clear();
        }
    }
    renderer.scrollback_cursor = messages.len();
    renderer.render_banner(term, provider_label, model_label)?;
    Ok("Chat cleared.".to_string())
}

use crate::llm::prompts;
use crate::llm::{ChatMessage, ChatRole};

/// Build provider history without truncating conversation or tool chains.
pub fn build_chat_history(messages: &[ChatMessage]) -> Vec<ChatMessage> {
    let mut out = Vec::with_capacity(messages.len() + 1);
    out.push(ChatMessage::new(
        ChatRole::System,
        prompts::system_prompt(),
    ));
    out.extend(messages.iter().cloned());
    out
}

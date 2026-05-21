use crate::llm::prompts;
use crate::llm::{ChatMessage, ChatRole};

use super::context::Context;
use super::message::Message;

/// Build the chat history to send to the LLM: system prompt + compacted messages.
/// Drops the oldest non-system messages to fit within the context budget.
pub fn build_chat_history(messages: &[Message], context: &Context) -> Vec<ChatMessage> {
    let mut contents: Vec<String> = vec![prompts::system_prompt().to_string()];
    for msg in messages {
        if !matches!(msg.role, ChatRole::System) {
            contents.push(msg.content.clone());
        }
    }

    let keep = context.fit(&contents);

    let mut history = vec![ChatMessage::new(
        ChatRole::System,
        prompts::system_prompt().to_string(),
    )];

    let mut msg_iter = messages
        .iter()
        .filter(|m| !matches!(m.role, ChatRole::System));

    let non_system_count = contents.len() - 1;
    let skip = non_system_count.saturating_sub(keep.saturating_sub(1));
    for _ in 0..skip {
        msg_iter.next();
    }

    for msg in msg_iter {
        history.push(msg.to_chat_message());
    }

    history
}

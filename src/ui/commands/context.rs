use crate::llm::ChatMessage;

/// Rough token estimation: ~4 chars per token for English/code.
const CHARS_PER_TOKEN: usize = 4;

/// Report current transcript token usage. No local context cap is applied.
pub fn run(messages: &[ChatMessage]) -> String {
    let used: usize = messages
        .iter()
        .map(|m| m.content.len().div_ceil(CHARS_PER_TOKEN))
        .sum();
    format!("Context: ~{used} tokens in transcript. No local token cap is applied.")
}

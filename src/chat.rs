use crate::llm::{ChatMessage, ChatRole};
use crate::llm::prompts;
use crate::ui::input::Message;

/// Rough token estimation: ~4 chars per token for English/code.
const CHARS_PER_TOKEN: usize = 4;

/// Default context window for local models (tokens).
const DEFAULT_MAX_TOKENS: usize = 8192;

/// Reserve this many tokens for the model's response.
const RESPONSE_RESERVE: usize = 2048;

/// Manages conversation context — tracks token budget and compacts when needed.
pub struct Context {
    /// Max context tokens the model supports.
    pub max_tokens: usize,
    /// Tokens reserved for the model's response.
    pub response_reserve: usize,
}

impl Default for Context {
    fn default() -> Self {
        Self { max_tokens: DEFAULT_MAX_TOKENS, response_reserve: RESPONSE_RESERVE }
    }
}

impl Context {
    #[must_use]
    pub fn new(max_tokens: usize) -> Self {
        Self { max_tokens, response_reserve: RESPONSE_RESERVE }
    }

    #[must_use]
    pub fn with_response_budget(mut self, tokens: usize) -> Self {
        self.response_reserve = tokens;
        self
    }

    /// Rough token count for a string.
    #[must_use]
    pub fn estimate_tokens(text: &str) -> usize {
        text.len().div_ceil(CHARS_PER_TOKEN)
    }

    /// How many tokens are available for messages (context window minus response reserve).
    #[must_use]
    pub fn budget(&self) -> usize {
        self.max_tokens.saturating_sub(self.response_reserve)
    }

    /// Given message contents (first = system prompt), return how many from the end
    /// fit the budget. Always keeps the system prompt.
    #[must_use]
    pub fn fit(&self, messages: &[String]) -> usize {
        if messages.len() <= 1 {
            return messages.len();
        }

        let budget = self.budget();
        let system_tokens = Self::estimate_tokens(&messages[0]);

        if system_tokens >= budget {
            return 1;
        }

        let remaining = budget - system_tokens;
        let mut used = 0usize;
        let mut kept = 0usize;

        // Walk newest → oldest, accumulate until budget exhausted.
        for i in (1..messages.len()).rev() {
            let tokens = Self::estimate_tokens(&messages[i]);
            if used + tokens > remaining {
                break;
            }
            used += tokens;
            kept += 1;
        }

        // 1 (system) + however many recent messages fit.
        1 + kept
    }
}

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

    let mut history = vec![ChatMessage {
        role: ChatRole::System,
        content: prompts::system_prompt().to_string(),
    }];

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

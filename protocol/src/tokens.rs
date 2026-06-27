//! Token usage tracking types.

use num_format::ToFormattedString;

/// Rough heuristic: ~3.8 UTF-8 chars per token for typical text.
pub const CHARS_PER_TOKEN: f64 = 3.8;

/// Lightweight token usage tracker.
#[derive(Debug, Clone, Default)]
pub struct TokenStats {
    pub sent: u64,
    pub received: u64,
    pub cached: u64,
    pub cost: f64,
    pub request_count: u64,
    pub context_length: u64,
}

impl TokenStats {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_request(
        &mut self,
        prompt_tokens: u32,
        completion_tokens: u32,
        cached_tokens: Option<u32>,
        cost: Option<f64>,
    ) {
        self.context_length = prompt_tokens as u64;
        self.sent += prompt_tokens as u64;
        self.received += completion_tokens as u64;
        self.cached += cached_tokens.unwrap_or(0) as u64;
        self.cost += cost.unwrap_or(0.0);
        self.request_count += 1;
    }

    pub fn record_estimate(&mut self, prompt_chars: usize, completion_chars: usize) {
        let chars_per_token = CHARS_PER_TOKEN;
        let estimated_prompt = (prompt_chars as f64 / chars_per_token).ceil() as u64;
        self.context_length = estimated_prompt;
        self.sent += estimated_prompt;
        self.received += (completion_chars as f64 / chars_per_token).ceil() as u64;
        self.request_count += 1;
    }

    pub fn set_context_estimate(&mut self, prompt_chars: usize) {
        let chars_per_token = CHARS_PER_TOKEN;
        self.context_length = (prompt_chars as f64 / chars_per_token).ceil() as u64;
    }

    /// Single-line summary for display.
    pub fn one_liner(&self) -> String {
        let mut parts = vec![
            format!("{} req", format_tokens(self.request_count)),
            format!("{} in", format_tokens(self.sent)),
            format!("{} out", format_tokens(self.received)),
        ];
        if self.cached > 0 {
            parts.push(format!("{} cached", format_tokens(self.cached)));
        }
        if self.cost > 0.0 {
            parts.push(format!("${:.2}", self.cost));
        }
        parts.join(" | ")
    }

    /// Reset cumulative fields for a new conversation.
    pub fn reset(&mut self) {
        self.sent = 0;
        self.received = 0;
        self.cached = 0;
        self.cost = 0.0;
        self.request_count = 0;
        self.context_length = 0;
    }
}

/// Format a token count with comma-separated thousands.
pub fn format_tokens(count: u64) -> String {
    count.to_formatted_string(&num_format::Locale::en)
}

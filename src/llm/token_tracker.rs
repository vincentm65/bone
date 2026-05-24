use num_format::ToFormattedString;

/// Lightweight token usage tracker.
///
/// Tracks cumulative tokens sent to and received from the LLM provider.
/// Provides a fallback estimator for providers that don't return usage data.
#[derive(Debug, Clone, Default)]
pub struct TokenStats {
    /// Tokens sent to the provider (prompt + tool definitions).
    pub sent: u64,
    /// Tokens received from the provider (completion).
    pub received: u64,
    /// Number of LLM requests made.
    pub request_count: u64,
    /// Prompt token count from the most recent request — i.e. the current
    /// context window size including system prompt, history, and tool defs.
    pub context_length: u64,
}

impl TokenStats {
    /// Create a new empty tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a completed request with real token counts from the provider.
    pub fn record_request(&mut self, prompt_tokens: u32, completion_tokens: u32) {
        self.context_length = prompt_tokens as u64;
        self.sent += prompt_tokens as u64;
        self.received += completion_tokens as u64;
        self.request_count += 1;
    }

    /// Record a request using a fallback character-based estimate.
    pub fn record_estimate(&mut self, prompt_chars: usize, completion_chars: usize) {
        // Rough heuristic: ~3.8 UTF-8 chars per token for typical text.
        let chars_per_token = 3.8;
        let estimated_prompt = (prompt_chars as f64 / chars_per_token).ceil() as u64;
        self.context_length = estimated_prompt;
        self.sent += estimated_prompt;
        self.received += (completion_chars as f64 / chars_per_token).ceil() as u64;
        self.request_count += 1;
    }

    #[allow(dead_code)]
    /// Total tokens across all requests.
    pub fn total(&self) -> u64 {
        self.sent + self.received
    }

    /// Format for display: "curr 1,234 | in 1,234 | out 340".
    pub fn display(&self) -> String {
        self.display_with_received_override(None)
    }

    /// Format for display, optionally overriding the cumulative received count.
    /// Used during streaming to show a live estimate until provider usage arrives.
    pub fn display_with_received_override(&self, received_override: Option<u64>) -> String {
        let received = received_override.unwrap_or(self.received);
        format!(
            "curr {} | in {} | out {}",
            format_tokens(self.context_length),
            format_tokens(self.sent),
            format_tokens(received)
        )
    }
}

/// Format a token count with comma-separated thousands.
pub fn format_tokens(count: u64) -> String {
    count.to_formatted_string(&num_format::Locale::en)
}

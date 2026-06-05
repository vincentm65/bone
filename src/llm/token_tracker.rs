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
    /// Tokens that were served from cache.
    pub cached: u64,
    /// Cumulative cost in USD.
    pub cost: f64,
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

    /// Set the current context length using the same fallback character-based estimate.
    ///
    /// This is for local transcript changes such as compaction, where no provider
    /// request has occurred yet, so cumulative sent/received totals must not change.
    pub fn set_context_estimate(&mut self, prompt_chars: usize) {
        // Rough heuristic: ~3.8 UTF-8 chars per token for typical text.
        let chars_per_token = 3.8;
        self.context_length = (prompt_chars as f64 / chars_per_token).ceil() as u64;
    }

    /// Total tokens across all requests.
    pub fn total(&self) -> u64 {
        self.sent + self.received
    }

    /// Multi-line summary for `/usage` command.
    pub fn summary(&self) -> String {
        let mut lines = Vec::new();
        lines.push("Conversation stats".to_string());
        lines.push(format!(
            "  Requests:  {}",
            format_tokens(self.request_count)
        ));
        lines.push(format!("  Tokens in: {}", format_tokens(self.sent)));
        lines.push(format!("  Tokens out: {}", format_tokens(self.received)));
        if self.cached > 0 {
            lines.push(format!("  Cached:    {}", format_tokens(self.cached)));
        }
        lines.push(format!(
            "  Context:   {} (current)",
            format_tokens(self.context_length)
        ));
        if self.cost > 0.0 {
            lines.push(format!("  Cost:      ${:.4}", self.cost));
        }
        if let Some(avg_in) = self.sent.checked_div(self.request_count)
            && let Some(avg_out) = self.received.checked_div(self.request_count)
        {
            lines.push(format!(
                "  Avg/req:   {} in / {} out",
                format_tokens(avg_in),
                format_tokens(avg_out)
            ));
        }
        lines.join("\n")
    }

    /// Single-line summary for `/clear` display.
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

    /// Format for display: "curr 1,234 | in 1,234 | out 340".
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

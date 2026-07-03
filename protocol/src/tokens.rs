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
    /// Calibration anchor: the last provider-reported prompt token count,
    /// paired with the char count of the request that produced it. Lets
    /// [`Self::anchored_context_estimate`] express a pending request as
    /// "last real count + estimated delta" instead of a raw chars/3.8 guess,
    /// which drifts badly on reasoning models (providers strip prior-turn
    /// thinking server-side, so a whole-history char estimate overshoots).
    pub context_anchor: Option<(u64, usize)>,
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

    /// Record the provider-reported prompt size together with the char
    /// estimate of the request that produced it.
    pub fn set_context_anchor(&mut self, prompt_tokens: u64, prompt_chars: usize) {
        self.context_anchor = Some((prompt_tokens, prompt_chars));
    }

    /// Drop the anchor. Call when the history is rewritten (compaction /
    /// `conversation.replace`), which invalidates the anchored char count.
    pub fn clear_context_anchor(&mut self) {
        self.context_anchor = None;
    }

    /// Estimate the context size of a pending request of `prompt_chars`
    /// chars. When an anchor is available, return the anchored token count
    /// adjusted by a char-estimate of the growth (or small shrink, e.g. a
    /// dropped transient turn message) since; without one fall back to the
    /// raw chars/`CHARS_PER_TOKEN` guess. History rewrites must clear the
    /// anchor rather than rely on this handling large shrinks.
    pub fn anchored_context_estimate(&self, prompt_chars: usize) -> u64 {
        let est = |chars: usize| (chars as f64 / CHARS_PER_TOKEN).ceil() as u64;
        match self.context_anchor {
            Some((tokens, chars)) if prompt_chars >= chars => tokens + est(prompt_chars - chars),
            Some((tokens, chars)) => tokens.saturating_sub(est(chars - prompt_chars)),
            None => est(prompt_chars),
        }
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
        self.context_anchor = None;
    }
}

/// Format a token count with comma-separated thousands.
pub fn format_tokens(count: u64) -> String {
    count.to_formatted_string(&num_format::Locale::en)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchored_estimate_without_anchor_is_raw_guess() {
        let stats = TokenStats::new();
        assert_eq!(stats.anchored_context_estimate(38), 10);
    }

    #[test]
    fn anchored_estimate_adds_growth_to_reported_tokens() {
        let mut stats = TokenStats::new();
        // Provider reported 50_000 tokens for a request we estimated at
        // 100_000 chars (raw guess would say ~26_316 — far off).
        stats.set_context_anchor(50_000, 100_000);
        assert_eq!(stats.anchored_context_estimate(100_000), 50_000);
        assert_eq!(stats.anchored_context_estimate(100_038), 50_010);
    }

    #[test]
    fn anchored_estimate_handles_small_shrink() {
        let mut stats = TokenStats::new();
        stats.set_context_anchor(50_000, 100_000);
        // A dropped transient turn message shrinks chars slightly; stay on
        // the anchored scale instead of reverting to the raw guess.
        assert_eq!(stats.anchored_context_estimate(99_962), 49_990);
    }

    #[test]
    fn reset_and_clear_drop_the_anchor() {
        let mut stats = TokenStats::new();
        stats.set_context_anchor(50_000, 100_000);
        stats.clear_context_anchor();
        assert_eq!(stats.anchored_context_estimate(38_000), 10_000);
        stats.set_context_anchor(50_000, 100_000);
        stats.reset();
        assert!(stats.context_anchor.is_none());
    }
}

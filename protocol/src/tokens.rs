//! Token usage tracking types.

use num_format::ToFormattedString;

/// Rough heuristic: ~3.8 UTF-8 chars per token for typical text.
pub const CHARS_PER_TOKEN: f64 = 3.8;

/// Vision-model image tokenization parameters.
///
/// VLMs tile an image into square patches, then merge a `merge_size`×
/// `merge_size` block of patches into one token. So the token count is driven
/// by the pixel grid aligned up to `patch_size * merge_size` per axis. `min`
/// and `max` clamp the result, matching how Qwen2-VL (and peers) bound a
/// single image's token budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageTokenProfile {
    /// Pixel size of one vision patch.
    pub patch_size: u32,
    /// Patches merged per axis into a single token.
    pub merge_size: u32,
    /// Lower bound on a single image's token count.
    pub min_tokens: u32,
    /// Upper bound on a single image's token count.
    pub max_tokens: u32,
    /// Cost assumed for an image whose pixel dimensions cannot be determined.
    pub unknown_tokens: u32,
}

impl Default for ImageTokenProfile {
    /// Qwen2-VL: 14px patch, 2×2 merge (28px stride), clamped to [4, 1280].
    fn default() -> Self {
        Self {
            patch_size: 14,
            merge_size: 2,
            min_tokens: 4,
            max_tokens: 1280,
            unknown_tokens: 512,
        }
    }
}

impl ImageTokenProfile {
    /// Token count for a single image of `width`×`height` pixels.
    pub fn tokens_for(&self, width: u32, height: u32) -> u32 {
        let stride = (self.patch_size * self.merge_size).max(1);
        let tiles = width
            .div_ceil(stride)
            .saturating_mul(height.div_ceil(stride));
        tiles.clamp(self.min_tokens, self.max_tokens)
    }
}

/// Estimate the token cost of one image. Uses `known` pixel dimensions (set by
/// the caller) when present, otherwise `sniffed` (parsed from the payload),
/// otherwise a fixed conservative cost.
pub fn estimate_image_tokens(
    known: Option<(u32, u32)>,
    sniffed: Option<(u32, u32)>,
    profile: &ImageTokenProfile,
) -> u32 {
    match known
        .filter(|(w, h)| *w > 0 && *h > 0)
        .or_else(|| sniffed.filter(|(w, h)| *w > 0 && *h > 0))
    {
        Some((w, h)) => profile.tokens_for(w, h),
        None => profile.unknown_tokens,
    }
}

/// Best-effort parse of pixel dimensions from a raw image payload, returning
/// `(width, height)`. Supports PNG, JPEG, GIF, and WebP; returns `None` for
/// anything else or truncated data. No decoding of the image body is performed.
pub fn parse_image_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    const PNG_SIG: &[u8] = &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    if data.len() >= 24 && data.starts_with(PNG_SIG) {
        let w = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
        let h = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
        return non_zero(w, h);
    }
    if data.len() >= 10 && data.starts_with(b"GIF8") {
        let w = u16::from_le_bytes([data[6], data[7]]);
        let h = u16::from_le_bytes([data[8], data[9]]);
        return non_zero(w as u32, h as u32);
    }
    if data.len() >= 30 && data.starts_with(b"RIFF") && data[8..12] == *b"WEBP" {
        return webp_dimensions(data);
    }
    if data.len() >= 4 && data[0] == 0xFF && data[1] == 0xD8 {
        return jpeg_dimensions(data);
    }
    None
}

fn non_zero(w: u32, h: u32) -> Option<(u32, u32)> {
    (w > 0 && h > 0).then_some((w, h))
}

fn webp_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    match &data[12..16] {
        // VP8X (extended): canvas size minus one, 3 bytes each, little-endian.
        b"VP8X" => {
            let w = u32::from_le_bytes([data[24], data[25], data[26], 0]) + 1;
            let h = u32::from_le_bytes([data[27], data[28], data[29], 0]) + 1;
            non_zero(w, h)
        }
        // VP8L (lossless): 14-bit (width-1) in the low bits, 14-bit (height-1)
        // in the next, packed little-endian over 4 bytes after the 0x2F tag.
        b"VP8L" => {
            if data[20] != 0x2F {
                return None;
            }
            let bits = u32::from_le_bytes([data[21], data[22], data[23], data[24]]);
            let w = (bits & 0x3FFF) + 1;
            let h = ((bits >> 14) & 0x3FFF) + 1;
            non_zero(w, h)
        }
        // VP8 (lossy): 16-bit even width/height after the keyframe start code.
        b"VP8 " => {
            let w = u16::from_le_bytes([data[26], data[27]]) as u32;
            let h = u16::from_le_bytes([data[28], data[29]]) as u32;
            non_zero(w, h)
        }
        _ => None,
    }
}

fn jpeg_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2usize;
    while i + 9 < data.len() {
        if data[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = data[i + 1];
        // SOF0..SOF15 except DHT (C4), JPG (C8), DAC (CC) carry the size.
        if (0xC0..=0xCF).contains(&marker) && marker != 0xC4 && marker != 0xC8 && marker != 0xCC {
            let h = u16::from_be_bytes([data[i + 5], data[i + 6]]) as u32;
            let w = u16::from_be_bytes([data[i + 7], data[i + 8]]) as u32;
            return non_zero(w, h);
        }
        // Standalone markers have no length-prefixed payload.
        if marker == 0xD8 || marker == 0xD9 || (0xD0..=0xD7).contains(&marker) {
            i += 2;
            continue;
        }
        let len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        if len < 2 {
            return None;
        }
        i += 2 + len;
    }
    None
}

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
#[path = "tokens_tests.rs"]
mod tokens_tests;

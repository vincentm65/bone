//! Cumulative token-usage tracking with a heuristic estimation fallback.
//!
//! Most types are re-exported from `bone-protocol`; only core-local
//! helpers remain here.

// Re-export wire-format types from protocol.
pub use bone_protocol::{
    CHARS_PER_TOKEN, ImageTokenProfile, TokenStats, estimate_image_tokens, format_tokens,
    parse_image_dimensions,
};

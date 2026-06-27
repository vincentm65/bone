//! Cumulative token-usage tracking with a heuristic estimation fallback.
//!
//! Most types are re-exported from `bone-protocol`; only core-local
//! helpers remain here.

// Re-export wire-format types from protocol.
pub use bone_protocol::{format_tokens, TokenStats, CHARS_PER_TOKEN};

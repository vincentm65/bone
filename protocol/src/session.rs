//! Session snapshot and per-provider usage types for the wire protocol.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct UsageProviderContext {
    pub provider: String,
    pub model: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cached_tokens: u64,
    pub cost: f64,
    pub request_count: u64,
}

/// A serializable snapshot of the cumulative session state, published after
/// every turn and on client attach so a frontend can mirror the daemon's
/// authoritative totals/ids without direct `RuntimeSession` reads.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SessionSnapshot {
    pub sent: u64,
    pub received: u64,
    pub cached: u64,
    pub cost: f64,
    pub request_count: u64,
    pub context_length: u64,
    pub transcript_len: usize,
    pub conversation_id: Option<i64>,
    pub session_seq: i64,
    pub provider_id: String,
    pub provider_model: String,
}

impl SessionSnapshot {
    /// Reconstruct a [`crate::tokens::TokenStats`] from the snapshot.
    pub fn to_token_stats(&self) -> crate::tokens::TokenStats {
        crate::tokens::TokenStats {
            sent: self.sent,
            received: self.received,
            cached: self.cached,
            cost: self.cost,
            request_count: self.request_count,
            context_length: self.context_length,
            // The anchor is calibration state private to the driving side; a
            // snapshot round-trip starts uncalibrated.
            context_anchor: None,
        }
    }
}

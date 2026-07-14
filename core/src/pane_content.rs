//! Pure-data pane types for core.
//!
//! Most types are re-exported from `bone-protocol`; only `KeyRequest`
//! (which carries a oneshot sender) stays core-local.

use tokio::sync::oneshot;

// Re-export wire-format types from protocol.
pub use bone_protocol::input::KeyEvent;
pub use bone_protocol::view::{PaneContent, PaneLineSpec, PaneSpanSpec};

/// A blocking request for the next terminal key.
#[derive(Debug)]
pub struct KeyRequest {
    pub reply: oneshot::Sender<KeyEvent>,
}

#[cfg(test)]
#[path = "pane_content_tests.rs"]
mod pane_content_tests;

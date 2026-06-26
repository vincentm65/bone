//! Frontend-agnostic runtime core.
//!
//! The `Driver` (see [`driver`]) is the single agent loop: it owns the
//! provider, tools, extensions, and session sink, and drives turns to
//! completion. `agent::run_agent` is a thin wrapper that builds a `Driver` with
//! an [`crate::tools::AutoApprovalGate`] and drains it. Interactive frontends
//! supply their own [`crate::tools::ApprovalGate`].
//!
//! This module has no dependency on `crate::ui` or ratatui — it is part of the
//! core that compiles with `--no-default-features`.

pub mod conn;
pub mod driver;
pub mod event;
pub mod session;
pub mod view;

pub use conn::{LocalConn, RuntimeConn, SocketConn};
pub use driver::{Driver, UsageRecord};
pub use event::{
    ApprovalReplyRegistry, ChannelApprovalGate, KeyReplyRegistry, RuntimeCommand, RuntimeEvent,
};
pub use session::RuntimeSession;
pub use view::{Component, ViewDiff, ViewModel};

/// Best-effort `String` from a panic payload (`&str`, `String`, or other),
/// returning a placeholder for non-string payloads.
pub fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&'static str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "<non-string panic payload>".to_string())
}

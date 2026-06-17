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

pub mod driver;
pub mod event;
pub mod view;

pub use driver::{Driver, UsageRecord};
pub use event::{
    ApprovalRequest, ChannelApprovalGate, KeyReplyRegistry, RuntimeCommand, RuntimeEvent,
};
pub use view::{Component, ViewDiff, ViewModel};

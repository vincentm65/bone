//! Tool-call approval decision — the pure runtime half of the client/server seam.
//!
//! Historically this logic was inlined in `agent::execute_tool_calls`
//! (`agent.rs:780-800`): for each tool call it (1) asked the extension hooks
//! whether to block, then (2) checked `ToolHandler::allows_call(mode, &call)`,
//! then (3) either queued the call for execution or emitted a "Tool skipped"
//! error. All three concerns were fused with `ToolHandler` and
//! `ExtensionManager`, making the policy untestable and non-injectable.
//!
//! This module extracts the *decision* into a pure function with no dependency
//! on `ToolHandler`, `ExtensionManager`, or Lua. The inputs (`blocked` from
//! hooks, `allows` from the approval mode) are supplied by the caller, so:
//!   - today: `execute_tool_calls` supplies them as before (behavior identical);
//!   - Step 2: a channel-driven `Driver` supplies `blocked`/`allows` from a
//!     client round-trip, and this function is unchanged.

use serde::{Deserialize, Serialize};

use crate::tools::{ApprovalMode, CommandSafety};

/// Outcome of deciding whether a single tool call may execute.
///
/// Serializable so an interactive decision can travel back from a frontend over
/// the runtime protocol as [`crate::runtime::RuntimeCommand::ApprovalReply`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallOutcome {
    /// Approved: queue for execution.
    Approve,
    /// An extension hook (or user advice) vetoed it; the string becomes the
    /// error-result content.
    Blocked(String),
    /// Approval mode disallows this call's safety level; not executed.
    Denied,
}

/// Pure approval decision.
///
/// A blocking hook verdict takes precedence over everything. Otherwise the
/// approval-mode allow-rule decides. This is the exact precedence that lived
/// inline in `execute_tool_calls`, now isolated so the policy is injectable
/// and unit-testable independent of the tool/extension machinery.
pub fn decide_call(blocked: Option<String>, allows: bool) -> CallOutcome {
    match blocked {
        Some(reason) => CallOutcome::Blocked(reason),
        None if allows => CallOutcome::Approve,
        None => CallOutcome::Denied,
    }
}

/// The "Tool skipped" error text for a mode-denied call.
///
/// Centralized as the single source of truth for this message (it was
/// duplicated inline). Uses `CommandSafety`'s `Debug` form — matching the
/// original byte-for-byte — and the mode's string label.
pub fn denied_message(mode: ApprovalMode, safety: CommandSafety) -> String {
    format!(
        "[exit_code=1] Tool skipped. Approval mode {} does not allow {:?}; continue using allowed read-only tools or report the limitation.",
        mode.mode_str(),
        safety
    )
}

/// Resolves a tool call to an outcome — the async approval seam.
///
/// The agent loop computes two inputs per call: `blocked` (the extension-hook
/// verdict) and `auto_allows` (the `ApprovalMode`/policy decision from
/// `ToolHandler::allows_call`). The gate turns those into a [`CallOutcome`].
///
/// The default impl reproduces the headless behavior exactly by delegating to
/// the pure [`decide_call`] — so `AutoApprovalGate` (and any gate that doesn't
/// override) is byte-for-byte identical to the old inline logic. An interactive
/// frontend (the TUI, or a remote client over RPC) overrides [`decide`] to
/// prompt the user when a call would otherwise be `Denied`, letting one loop
/// serve both auto and interactive approval.
///
/// [`decide`]: ApprovalGate::decide
#[async_trait::async_trait]
pub trait ApprovalGate: Send + Sync {
    async fn decide(
        &self,
        blocked: Option<String>,
        auto_allows: bool,
        call: &crate::tools::ToolCall,
    ) -> CallOutcome {
        let _ = call;
        decide_call(blocked, auto_allows)
    }
}

/// The non-interactive gate: outcome is purely `decide_call(blocked, auto_allows)`.
/// Used by the headless agent and by tests. Behavior is identical to the
/// pre-Driver inline approval logic.
pub struct AutoApprovalGate;

impl ApprovalGate for AutoApprovalGate {}

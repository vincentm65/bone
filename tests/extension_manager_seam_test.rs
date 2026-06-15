//! Step 2: prove the `ExtensionManager::unloaded()` constructor seam.
//!
//! The agent loop calls only dispatch methods on `ExtensionManager`, and every
//! one of them early-returns when `!self.loaded`. Until now the *only* way to
//! build a manager was the private `from_arc` (which needs a real booted Lua
//! VM), so the loop could not be driven or unit-tested in isolation.
//!
//! `unloaded()` exposes the no-op construction that `boot()` already used
//! internally as its engine-failure fallback. These tests confirm an unloaded
//! manager is a provably inert stub: no hooks fire, tool calls aren't blocked,
//! and all the read-only accessors return empty/default state.

use bone::ext::{EventDispatchResult, ExtensionManager};

#[test]
fn unloaded_reports_unavailable() {
    let m = ExtensionManager::unloaded();
    // engine_ok = false → extensions effectively disabled.
    assert!(!m.is_available());
}

#[test]
fn unloaded_tool_call_is_not_blocked() {
    let m = ExtensionManager::unloaded();
    // No hook can veto when nothing is loaded → Continue, never Blocked.
    let res = m.dispatch_tool_call(
        "write_file",
        "call_1",
        &serde_json::json!({}),
        "danger",
    );
    assert!(matches!(res, EventDispatchResult::Continue));
}

#[test]
fn unloaded_tool_result_is_a_noop() {
    let m = ExtensionManager::unloaded();
    // Must not panic; nothing to assert — no hooks to observe.
    m.dispatch_tool_result("write_file", "call_1", false);
}

#[test]
fn unloaded_simple_dispatch_is_a_noop() {
    let m = ExtensionManager::unloaded();
    // session_start / message / mode_change dispatch must not panic.
    m.dispatch_simple("session_start", serde_json::json!({}));
    m.dispatch_simple("message", serde_json::json!({"role": "assistant"}));
}

#[test]
fn unloaded_has_no_commands_or_subagents() {
    let m = ExtensionManager::unloaded();
    assert!(m.commands().is_empty());
    assert!(m.subagent_names().is_empty());
}

#[test]
fn unloaded_snapshots_are_defaults() {
    // Snapshots reflect empty/default boot state, not a loaded config.
    let m = ExtensionManager::unloaded();
    assert!(m.config_snapshot().approval_mode.is_none());
    assert!(m.config_snapshot().status_show.is_empty());
    assert!(m.theme_snapshot().user_msg.is_none());
    assert!(m.keymap_snapshot().normal.is_empty());
    assert!(m.keymap_snapshot().insert.is_empty());
}

#[test]
fn unloaded_is_independently_cloneable() {
    // Clone is the mechanism used to hand a manager to spawn_blocking.
    // An unloaded clone stays unloaded and inert.
    let m = ExtensionManager::unloaded();
    let clone = m.clone();
    let res = clone.dispatch_tool_call("shell", "x", &serde_json::json!({}), "danger");
    assert!(matches!(res, EventDispatchResult::Continue));
    assert!(!clone.is_available());
}

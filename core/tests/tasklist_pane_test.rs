//! Regression: a host-stateful tool that returns a `pane` envelope (task_list)
//! must push that pane into the shared UiState handle the TUI drains, otherwise
//! the live pane is parsed into `ToolResult.pane_page` but never rendered.

mod common;

use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use bone_core::runtime::view::ViewDiff;
use bone_core::tools::types::ToolCall;

const TASK_LIST: &str = include_str!("../defaults/lua/tools/task_list.lua");

fn test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn lock_tests() -> std::sync::MutexGuard<'static, ()> {
    test_lock().lock().unwrap_or_else(|e| e.into_inner())
}

fn boot(config_dir: &std::path::Path) -> bone_core::ext::BootedTools {
    let tools_dir = config_dir.join("lua/tools");
    std::fs::create_dir_all(&tools_dir).unwrap();
    std::fs::write(tools_dir.join("task_list.lua"), TASK_LIST).unwrap();

    let mut custom = bone_core::config::custom::CustomConfigs::default();
    bone_core::ext::boot_with_tools(
        config_dir,
        config_dir,
        &mut custom,
        false,
        bone_core::ext::BootOptions::default(),
        "test-model",
        "TestProvider",
    )
}

fn run(
    booted: &bone_core::ext::BootedTools,
    args: serde_json::Value,
) -> bone_core::tools::types::ToolResult {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let call = ToolCall {
        id: "call-tl".to_string(),
        name: "task_list".to_string(),
        arguments: args,
    };
    let mut results = rt
        .block_on(async {
            tokio::time::timeout(
                Duration::from_secs(10),
                booted.tools.execute_all(vec![call], 0),
            )
            .await
        })
        .expect("task_list timed out");
    assert_eq!(results.len(), 1);
    results.pop().unwrap()
}

#[test]
fn tasklist_create_pushes_pane_to_shared_ui() {
    let _guard = lock_tests();
    let config_dir = common::temp_dir("tasklist-pane-create");
    let booted = boot(&config_dir);

    let res = run(
        &booted,
        serde_json::json!({
            "action": "write",
            "name": "Demo",
            "tasks": ["alpha", "beta"],
        }),
    );
    assert!(!res.is_error, "tool errored: {}", res.content);
    assert!(res.pane_page.is_some(), "tool result should carry a pane");

    // The pane must reach the handle the TUI drains, as an upsert of the
    // `task_list` source.
    let diffs = booted.manager.drain_view_diffs();
    assert_eq!(diffs.len(), 1, "expected exactly one pane diff");
    match &diffs[0] {
        ViewDiff::Upsert { component } => {
            assert_eq!(component.id(), "task_list");
            let pc = component.as_pane_content().expect("pane content");
            assert!(pc.title.contains("Demo"));
            assert_eq!(pc.lines.len(), 2);
        }
        other => panic!("expected Upsert, got {other:?}"),
    }
}

#[test]
fn tasklist_complete_updates_state_through_ctx() {
    let _guard = lock_tests();
    let config_dir = common::temp_dir("tasklist-complete");
    let booted = boot(&config_dir);

    run(
        &booted,
        serde_json::json!({
            "action": "write",
            "tasks": ["alpha", {"text": "beta", "status": "in_progress"}],
        }),
    );
    let _ = booted.manager.drain_view_diffs();

    let res = run(&booted, serde_json::json!({ "action": "complete" }));
    assert!(!res.is_error, "tool errored: {}", res.content);
    assert_eq!(res.content, "All tasks complete.");

    let state: serde_json::Value =
        serde_json::from_str(res.state.as_deref().expect("complete should return state")).unwrap();
    assert!(
        state["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|task| task["status"] == "done")
    );
}

#[test]
fn tasklist_kill_pushes_pane_removal() {
    let _guard = lock_tests();
    let config_dir = common::temp_dir("tasklist-pane-kill");
    let booted = boot(&config_dir);

    // Seed a list, then drain the write diff so we isolate the clear diff.
    run(
        &booted,
        serde_json::json!({ "action": "write", "tasks": ["x"] }),
    );
    let _ = booted.manager.drain_view_diffs();

    let res = run(&booted, serde_json::json!({ "action": "clear" }));
    assert!(!res.is_error, "tool errored: {}", res.content);

    let diffs = booted.manager.drain_view_diffs();
    assert_eq!(diffs.len(), 1, "expected exactly one pane diff");
    match &diffs[0] {
        ViewDiff::Remove { id } => assert_eq!(id, "task_list"),
        other => panic!("expected Remove, got {other:?}"),
    }
}

/// Two independent boots (parent vs subagent, or two conversation actors)
/// must not share `ctx.state` — otherwise a child task_list write overwrites
/// the parent's checklist.
#[test]
fn tasklist_state_is_isolated_across_boots() {
    let _guard = lock_tests();
    let parent_dir = common::temp_dir("tasklist-iso-parent");
    let child_dir = common::temp_dir("tasklist-iso-child");
    let parent = boot(&parent_dir);
    let child = boot(&child_dir);

    assert!(
        !std::sync::Arc::ptr_eq(&parent.tools.shared_state, &child.tools.shared_state),
        "each boot must allocate its own shared_state Arc"
    );

    run(
        &parent,
        serde_json::json!({ "action": "write", "tasks": ["parent-only"] }),
    );
    run(
        &child,
        serde_json::json!({ "action": "write", "tasks": ["child-only"] }),
    );

    let parent_raw = parent
        .tools
        .shared_state
        .lock()
        .unwrap()
        .get("task_list")
        .cloned();
    let child_raw = child
        .tools
        .shared_state
        .lock()
        .unwrap()
        .get("task_list")
        .cloned();

    let parent_state: serde_json::Value =
        serde_json::from_str(parent_raw.as_deref().expect("parent list")).unwrap();
    let child_state: serde_json::Value =
        serde_json::from_str(child_raw.as_deref().expect("child list")).unwrap();

    assert_eq!(parent_state["tasks"][0]["text"], "parent-only");
    assert_eq!(child_state["tasks"][0]["text"], "child-only");
    assert_ne!(
        parent_raw, child_raw,
        "parent and child maps must hold distinct checklist blobs"
    );
}

/// `/new` / `/clear` / load call `clear_host_state` so the next turn's
/// before_turn and tool path don't resurrect a stale list.
#[test]
fn tasklist_clear_host_state_drops_ctx_and_state_map() {
    let _guard = lock_tests();
    let config_dir = common::temp_dir("tasklist-clear-host");
    let mut booted = boot(&config_dir);

    run(
        &booted,
        serde_json::json!({ "action": "write", "tasks": ["stale"] }),
    );
    // Mirror what the driver does after a successful stateful write.
    if let Some(state) = booted
        .tools
        .shared_state
        .lock()
        .unwrap()
        .get("task_list")
        .cloned()
    {
        booted.tools.state_map.set("task_list", "default", state);
    }

    {
        let mut snapshots = booted.tools.snapshots.write().unwrap();
        snapshots.record("stale.txt", "stale", Some(&[1]));
    }

    booted.tools.clear_host_state();

    assert!(
        booted
            .tools
            .shared_state
            .lock()
            .unwrap()
            .get("task_list")
            .is_none(),
        "ctx.state map must be empty after clear_host_state"
    );
    assert!(
        booted.tools.state_map.get("task_list", "default").is_none(),
        "state_map must be empty after clear_host_state"
    );

    assert!(
        booted
            .tools
            .snapshots
            .read()
            .unwrap()
            .head("stale.txt")
            .is_none(),
        "snapshots must be empty after clear_host_state"
    );

    // A subsequent complete without a list should fail cleanly, not complete
    // a ghost list. Lua ERROR strings return as content (is_error may stay false).
    let res = run(&booted, serde_json::json!({ "action": "complete" }));
    assert!(
        res.content.contains("No active task list"),
        "unexpected complete result: {}",
        res.content
    );
    assert!(res.state.is_none(), "ghost list must not return state");
}

#[test]
fn tasklist_invalid_action_mentions_complete() {
    let _guard = lock_tests();
    let config_dir = common::temp_dir("tasklist-bad-action");
    let booted = boot(&config_dir);

    let res = run(&booted, serde_json::json!({ "action": "nope" }));
    assert!(
        res.content.contains("advance") && res.content.contains("complete"),
        "error should list advance and complete: {}",
        res.content
    );
}

/// action=advance closes the current step and opens the next without a full rewrite.
#[test]
fn tasklist_advance_marks_current_done_and_starts_next() {
    let _guard = lock_tests();
    let config_dir = common::temp_dir("tasklist-advance");
    let booted = boot(&config_dir);

    run(
        &booted,
        serde_json::json!({
            "action": "write",
            "tasks": [
                {"text": "one", "status": "in_progress"},
                "two",
                "three",
            ],
        }),
    );

    let res = run(&booted, serde_json::json!({ "action": "advance" }));
    assert!(!res.is_error, "advance errored: {}", res.content);
    assert_eq!(res.content, "1/3 done");

    let state: serde_json::Value =
        serde_json::from_str(res.state.as_deref().expect("advance returns state")).unwrap();
    let tasks = state["tasks"].as_array().unwrap();
    assert_eq!(tasks[0]["status"], "done");
    assert_eq!(tasks[1]["status"], "in_progress");
    assert_eq!(tasks[2]["status"], "pending");

    let res2 = run(&booted, serde_json::json!({ "action": "advance" }));
    let state2: serde_json::Value = serde_json::from_str(res2.state.as_deref().unwrap()).unwrap();
    assert_eq!(state2["tasks"][1]["status"], "done");
    assert_eq!(state2["tasks"][2]["status"], "in_progress");

    let res3 = run(&booted, serde_json::json!({ "action": "advance" }));
    assert_eq!(res3.content, "All tasks complete.");
    let state3: serde_json::Value = serde_json::from_str(res3.state.as_deref().unwrap()).unwrap();
    assert!(
        state3["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["status"] == "done")
    );
}

/// write with a later in_progress auto-closes earlier unfinished items so the
/// list does not stall with forgotten prior steps still pending.
#[test]
fn tasklist_write_closes_prior_steps_when_later_in_progress() {
    let _guard = lock_tests();
    let config_dir = common::temp_dir("tasklist-auto-close-prior");
    let booted = boot(&config_dir);

    let res = run(
        &booted,
        serde_json::json!({
            "action": "write",
            "tasks": [
                "alpha",
                "beta",
                {"text": "gamma", "status": "in_progress"},
            ],
        }),
    );
    assert!(!res.is_error, "write errored: {}", res.content);
    assert_eq!(res.content, "2/3 done");

    let state: serde_json::Value = serde_json::from_str(res.state.as_deref().unwrap()).unwrap();
    assert_eq!(state["tasks"][0]["status"], "done");
    assert_eq!(state["tasks"][1]["status"], "done");
    assert_eq!(state["tasks"][2]["status"], "in_progress");
}

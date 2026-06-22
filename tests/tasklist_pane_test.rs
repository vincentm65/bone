//! Regression: a host-stateful tool that returns a `pane` envelope (task_list)
//! must push that pane into the shared UiState handle the TUI drains, otherwise
//! the live pane is parsed into `ToolResult.pane_page` but never rendered.

mod common;

use std::time::Duration;

use bone::runtime::view::ViewDiff;
use bone::tools::types::ToolCall;

const TASK_LIST: &str = include_str!("fixtures/task_list.lua");

fn boot(config_dir: &std::path::Path) -> bone::ext::BootedTools {
    let tools_dir = config_dir.join("lua/tools");
    std::fs::create_dir_all(&tools_dir).unwrap();
    std::fs::write(tools_dir.join("task_list.lua"), TASK_LIST).unwrap();

    let mut custom = bone::config::custom::CustomConfigs::default();
    bone::ext::boot_with_tools(
        config_dir,
        config_dir,
        &mut custom,
        false,
        bone::ext::BootOptions::default(),
        "test-model",
        "TestProvider",
    )
}

fn run(booted: &bone::ext::BootedTools, args: serde_json::Value) -> bone::tools::types::ToolResult {
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
fn tasklist_kill_pushes_pane_removal() {
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

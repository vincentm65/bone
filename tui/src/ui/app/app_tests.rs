use super::{
    WireTools, apply_queue_nav_key, background_pane_needs_refresh, configured_input_style,
    edit_diff_message, job_snapshot_messages, should_open_agent_log,
};
use crate::ui::input::InputState;
use crate::ui::render::InputPreset;
use crossterm::event::{KeyCode, KeyModifiers};
use std::collections::VecDeque;

#[test]
fn config_preset_override_preserves_explicit_lua_input_customization() {
    let snapshot = crate::ext::snapshots::InputStyleSnapshot {
        preset: Some("lines".into()),
        prefix: Some("λ ".into()),
        horizontal_padding: Some(3),
        vertical_padding: Some(2),
        fill: Some(false),
        ..Default::default()
    };

    let custom = configured_input_style(&snapshot, None);
    assert_eq!(custom.preset, InputPreset::Lines);

    let filled = configured_input_style(&snapshot, Some("filled"));
    assert_eq!(filled.preset, InputPreset::Filled);
    assert_eq!(filled.prefix, "λ ");
    assert_eq!(filled.horizontal_padding, 3);
    assert_eq!(filled.vertical_padding, 2);
    assert!(!filled.fill);

    let box_defaults = configured_input_style(
        &crate::ext::snapshots::InputStyleSnapshot::default(),
        Some("box"),
    );
    assert_eq!(box_defaults.preset, InputPreset::Box);
    assert_eq!(box_defaults.horizontal_padding, 1);
    assert!(!box_defaults.fill);
}

#[test]
fn finished_process_refreshes_visible_pane_for_removal() {
    assert!(background_pane_needs_refresh(false, true, false));
    assert!(!background_pane_needs_refresh(false, false, false));
}

#[test]
fn agent_log_enter_opens_log_with_empty_input() {
    assert!(should_open_agent_log(&InputState::default()));
}

#[test]
fn agent_log_enter_submits_nonempty_input() {
    let mut input = InputState::default();
    input.buffer = "queue this message".into();

    assert!(!should_open_agent_log(&input));
}

#[test]
fn queue_enter_with_input_falls_through_to_submission() {
    let mut queue = VecDeque::from(["queued".to_string()]);
    let mut selected = 0;
    let mut editing = None;
    let mut input = InputState::default();
    input.buffer = "typed message".into();

    assert!(!apply_queue_nav_key(
        KeyCode::Enter,
        KeyModifiers::NONE,
        &mut queue,
        &mut selected,
        &mut editing,
        &mut input,
    ));
    assert_eq!(queue.front().map(String::as_str), Some("queued"));
}

#[test]
fn queue_navigation_still_works_with_input() {
    let mut queue = VecDeque::from(["first".to_string(), "second".to_string()]);
    let mut selected = 0;
    let mut editing = None;
    let mut input = InputState::default();
    input.buffer = "typed message".into();

    assert!(apply_queue_nav_key(
        KeyCode::Down,
        KeyModifiers::NONE,
        &mut queue,
        &mut selected,
        &mut editing,
        &mut input,
    ));
    assert_eq!(selected, 1);
}

fn job_with_events(events: Vec<crate::ext::jobs::JobEvent>) -> crate::ext::jobs::Job {
    crate::ext::jobs::Job {
        id: "job-1".into(),
        agent: "worker".into(),
        task: "do work".into(),
        title: "Work".into(),
        status: crate::ext::jobs::JobStatus::Running,
        result: None,
        started_at: 0,
        finished_at: None,
        consumed: false,
        token_sent: 0,
        token_received: 0,
        result_file: None,
        max_concurrency: 1,
        activity: None,
        trace: Vec::new(),
        events,
        transcript: None,
        scope: None,
        cancel_flag: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    }
}

#[test]
fn job_snapshot_correlates_shell_result_and_ignores_incremental_output() {
    let events = vec![
        crate::ext::jobs::JobEvent {
            event: crate::runtime::RuntimeEvent::ToolCall {
                id: "call-1".into(),
                name: "shell".into(),
                summary: "shell: echo hi".into(),
                arguments: serde_json::json!({ "command": "echo hi" }),
            },
            edit_preview: None,
        },
        crate::ext::jobs::JobEvent {
            event: crate::runtime::RuntimeEvent::ToolOutput {
                call_id: "call-1".into(),
                content: "h".into(),
                stderr: false,
            },
            edit_preview: None,
        },
        crate::ext::jobs::JobEvent {
            event: crate::runtime::RuntimeEvent::ToolResult {
                name: "shell".into(),
                call_id: "call-1".into(),
                is_error: false,
                content: "hi\n".into(),
            },
            edit_preview: None,
        },
    ];

    let rows = job_snapshot_messages(&job_with_events(events), &WireTools::default());
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].content, "hi\n");
    assert_eq!(rows[1].tool.as_ref().unwrap().label, "shell echo hi");
    assert!(rows[1].tool.as_ref().unwrap().is_shell);
}

#[test]
fn job_snapshot_renders_captured_edit_preview_once() {
    let diff = "\n--- a/file\n+++ b/file\n";
    let events = vec![
        crate::ext::jobs::JobEvent {
            event: crate::runtime::RuntimeEvent::ToolCall {
                id: "call-1".into(),
                name: "edit_file".into(),
                summary: "edit_file: file".into(),
                arguments: serde_json::json!({
                    "path": "file",
                    "old_text": "old",
                    "new_text": "new"
                }),
            },
            edit_preview: Some(diff.into()),
        },
        crate::ext::jobs::JobEvent {
            event: crate::runtime::RuntimeEvent::ToolResult {
                name: "edit_file".into(),
                call_id: "call-1".into(),
                is_error: false,
                content: format!("Edited: file{diff}"),
            },
            edit_preview: None,
        },
    ];

    let rows = job_snapshot_messages(&job_with_events(events), &WireTools::default());
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].content, diff);
    assert!(rows[1].tool.is_none());
}

#[test]
fn restored_edit_result_uses_current_prefix() {
    let row = edit_diff_message("edit_file", false, "Edited: file\n--- a/file\n+++ b/file\n")
        .expect("edit diff");
    assert_eq!(row.content, "\n--- a/file\n+++ b/file\n");
    assert!(edit_diff_message("edit_file", true, "Edited: file\n--- diff").is_none());
}

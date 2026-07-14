use super::{WireTools, edit_diff_message, job_snapshot_messages, should_open_agent_log};
use crate::ui::input::InputState;

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

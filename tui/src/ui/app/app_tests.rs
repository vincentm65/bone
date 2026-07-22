use super::{
    App, ConfigView, WireTools, apply_queue_nav_key, background_pane_needs_refresh,
    config_rejection_message, configured_input_style, edit_diff_message, idle_state_needs_redraw,
    job_snapshot_messages, lua_config_available, parse_config_value, prepare_streaming_replay,
    render_config_page, run_insertion_lifecycle, should_open_agent_log, take_pending_config,
    terminal_dimensions_changed,
};
use crate::ui::input::InputState;
use crate::ui::render::InputPreset;
use crossterm::event::{KeyCode, KeyModifiers};
use std::collections::VecDeque;

#[test]
fn terminal_dimension_changes_require_a_prior_size() {
    assert!(!terminal_dimensions_changed(None, (80, 24)));
    assert!(!terminal_dimensions_changed(Some((80, 24)), (80, 24)));
    assert!(terminal_dimensions_changed(Some((80, 24)), (100, 24)));
    assert!(terminal_dimensions_changed(Some((80, 24)), (80, 30)));
    assert!(terminal_dimensions_changed(Some((80, 24)), (100, 30)));
}

#[test]
fn streaming_replay_resets_source_offset_and_accounts_for_message() {
    let mut renderer = crate::ui::render::Renderer::new();
    renderer.scrollback_cursor = 2;
    renderer.streaming_source_flushed = 37;

    prepare_streaming_replay(&mut renderer);

    assert_eq!(renderer.scrollback_cursor, 3);
    assert_eq!(renderer.streaming_source_flushed, 0);
}

#[test]
fn insertion_lifecycle_restores_a_windows_cleared_viewport() {
    const VIEWPORT_ROWS: [&str; 7] = [
        "top separator",
        "pane",
        "autocomplete",
        "input field",
        "bottom separator",
        "running shell",
        "status row",
    ];

    #[derive(Default)]
    struct Model {
        events: Vec<&'static str>,
        viewport: Vec<&'static str>,
        scrollback: Vec<&'static str>,
    }

    let mut model = Model::default();
    run_insertion_lifecycle(
        &mut model,
        |model| {
            model.events.push("size/draw");
            model.viewport.extend(VIEWPORT_ROWS);
            Ok(())
        },
        |model| {
            model.events.push("insert");
            model.scrollback.push("message");
            #[cfg(windows)]
            model.viewport.clear();
            Ok(())
        },
        |model| {
            #[cfg(windows)]
            {
                model.events.push("Windows draw");
                model.viewport.extend(VIEWPORT_ROWS);
            }
            #[cfg(not(windows))]
            let _ = model;
            Ok(())
        },
    )
    .unwrap();

    #[cfg(windows)]
    assert_eq!(model.events, ["size/draw", "insert", "Windows draw"]);
    #[cfg(not(windows))]
    assert_eq!(model.events, ["size/draw", "insert"]);
    assert_eq!(model.viewport, VIEWPORT_ROWS);
    assert_eq!(model.scrollback, ["message"]);
}

#[test]
fn insertion_callers_preserve_reset_resize_insert_restore_order() {
    for (name, resets_input) in [
        ("multiline submit", true),
        ("inline command", true),
        ("daemon turn", false),
        ("stream fragment", false),
    ] {
        let mut events = Vec::new();
        if resets_input {
            events.push("reset");
        }
        run_insertion_lifecycle(
            &mut events,
            |events| {
                events.push("size/draw");
                Ok(())
            },
            |events| {
                events.push("insert");
                Ok(())
            },
            |events| {
                #[cfg(windows)]
                events.push("Windows draw");
                #[cfg(not(windows))]
                let _ = events;
                Ok(())
            },
        )
        .unwrap();

        let mut expected = if resets_input {
            vec!["reset", "size/draw", "insert"]
        } else {
            vec!["size/draw", "insert"]
        };
        #[cfg(windows)]
        expected.push("Windows draw");
        assert_eq!(events, expected, "{name}");
    }
}

#[test]
fn stale_resize_rebuild_precedes_width_sensitive_stream_insertion() {
    #[derive(Default)]
    struct Model {
        events: Vec<&'static str>,
        tracked_size: Option<(u16, u16)>,
        insertion_width: u16,
        streaming_offset: usize,
        scrollback_cursor: usize,
    }

    let mut model = Model {
        tracked_size: Some((80, 24)),
        streaming_offset: 37,
        scrollback_cursor: 2,
        ..Model::default()
    };
    let current_size = (42, 18);
    run_insertion_lifecycle(
        &mut model,
        |model| {
            if terminal_dimensions_changed(model.tracked_size, current_size) {
                model.events.push("rebuild");
                let mut renderer = crate::ui::render::Renderer::new();
                renderer.scrollback_cursor = model.scrollback_cursor;
                renderer.streaming_source_flushed = model.streaming_offset;
                prepare_streaming_replay(&mut renderer);
                model.scrollback_cursor = renderer.scrollback_cursor;
                model.streaming_offset = renderer.streaming_source_flushed;
                model.tracked_size = Some(current_size);
            }
            model.events.push("size/draw");
            model.insertion_width = model.tracked_size.unwrap().0;
            Ok(())
        },
        |model| {
            model.events.push("insert");
            assert_eq!(model.insertion_width, current_size.0);
            model.streaming_offset += 11;
            Ok(())
        },
        |model| {
            #[cfg(windows)]
            model.events.push("Windows draw");
            #[cfg(not(windows))]
            let _ = model;
            Ok(())
        },
    )
    .unwrap();

    let mut expected = vec!["rebuild", "size/draw", "insert"];
    #[cfg(windows)]
    expected.push("Windows draw");
    assert_eq!(model.events, expected);
    assert_eq!(model.tracked_size, Some(current_size));
    assert_eq!(model.insertion_width, 42);
    assert_eq!(model.scrollback_cursor, 3);
    assert_eq!(model.streaming_offset, 11);
}

#[test]
fn bundled_lua_config_is_available_for_interactive_dispatch() {
    assert!(lua_config_available(&[(
        "config".into(),
        "edit configuration".into(),
    )]));
    assert!(!lua_config_available(&[(
        "history".into(),
        "browse conversations".into(),
    )]));
}

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

fn config_view() -> ConfigView {
    let field = bone_protocol::SettingDefinition {
        path: "general.approval".into(),
        key: "approval".into(),
        label: "Approval mode".into(),
        value_type: "enum".into(),
        options: vec!["safe".into(), "danger".into()],
        default: serde_json::json!("safe"),
        value: Some(serde_json::json!("safe")),
        integer: None,
        min: None,
        max: None,
        reload_behavior: "immediate".into(),
    };
    ConfigView {
        schema: Some(bone_protocol::ConfigSchema {
            pages: vec![bone_protocol::ConfigPage {
                namespace: "general".into(),
                title: "General".into(),
                fields: vec![field],
                pages: Vec::new(),
            }],
        }),
        snapshot: Some(bone_protocol::ConfigSnapshot {
            revision: 7,
            values: serde_json::json!({ "general": { "approval": "danger" } }),
            providers: Vec::new(),
            active_provider: String::new(),
            disabled_tools: Vec::new(),
            disabled_commands: Vec::new(),
        }),
    }
}

#[test]
fn config_page_uses_schema_and_authoritative_snapshot() {
    let output = render_config_page(&config_view(), Some("general")).unwrap();
    assert!(output.contains("General"));
    assert!(output.contains("general.approval = danger [safe | danger]"));
}

#[test]
fn config_value_validation_uses_schema_options() {
    let view = config_view();
    let field = view.field("general.approval").unwrap();
    assert_eq!(
        parse_config_value(field, "safe").unwrap(),
        serde_json::json!("safe")
    );
    assert_eq!(
        parse_config_value(field, "invalid").unwrap_err(),
        "expected one of: safe, danger"
    );
}

#[test]
fn config_value_validation_enforces_number_bounds_and_integer_shape() {
    let mut field = config_view().field("general.approval").unwrap().clone();
    field.value_type = "number".into();
    field.integer = Some(true);
    field.min = Some(1.0);
    field.max = Some(3.0);

    assert_eq!(
        parse_config_value(&field, "2").unwrap(),
        serde_json::json!(2)
    );
    assert_eq!(
        parse_config_value(&field, "1.5").unwrap_err(),
        "expected an integer"
    );
    assert_eq!(
        parse_config_value(&field, "0").unwrap_err(),
        "must be at least 1"
    );
    assert_eq!(
        parse_config_value(&field, "4").unwrap_err(),
        "must be at most 3"
    );

    field.integer = None;
    assert_eq!(
        parse_config_value(&field, "2.5").unwrap(),
        serde_json::json!(2.5)
    );
    assert_eq!(
        parse_config_value(&field, "NaN").unwrap_err(),
        "expected a finite number"
    );
}

#[test]
fn config_bool_aliases_are_ascii_case_insensitive() {
    let mut field = config_view().field("general.approval").unwrap().clone();
    field.value_type = "bool".into();

    assert_eq!(
        parse_config_value(&field, "TRUE").unwrap(),
        serde_json::json!(true)
    );
    assert_eq!(
        parse_config_value(&field, "Off").unwrap(),
        serde_json::json!(false)
    );
    assert_eq!(
        parse_config_value(&field, "1").unwrap_err(),
        "expected true/false, on/off, or yes/no"
    );
}

#[test]
fn config_page_lookup_and_render_are_recursive() {
    let mut view = config_view();
    let field = view.schema.as_ref().unwrap().pages[0].fields[0].clone();
    view.schema.as_mut().unwrap().pages[0].fields.clear();
    view.schema.as_mut().unwrap().pages[0].pages = vec![bone_protocol::ConfigPage {
        namespace: "extensions.example".into(),
        title: "Example extension".into(),
        fields: vec![bone_protocol::SettingDefinition {
            path: "extensions.example.mode".into(),
            key: "mode".into(),
            ..field
        }],
        pages: Vec::new(),
    }];
    view.snapshot.as_mut().unwrap().values =
        serde_json::json!({ "extensions": { "example": { "mode": "danger" } } });

    assert!(view.field("extensions.example.mode").is_some());
    let output = render_config_page(&view, Some("extensions.example")).unwrap();
    assert!(output.contains("extensions.example.mode = danger"));
}

#[test]
fn config_rejection_message_includes_pending_path_when_known() {
    assert_eq!(
        config_rejection_message(Some("general.approval".into()), "permission denied"),
        "Configuration change for general.approval rejected: permission denied"
    );
    assert_eq!(
        config_rejection_message(None, "permission denied"),
        "Configuration change rejected: permission denied"
    );
}

#[test]
fn idle_config_revision_change_requests_redraw_without_new_messages() {
    assert!(idle_state_needs_redraw(false, 4, 4, 7, 8));
    assert!(!idle_state_needs_redraw(false, 4, 4, 7, 7));
}

#[test]
fn rejected_config_change_restores_approval_and_requests_snapshot() {
    let _guard = crate::ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let previous = std::env::var_os("BONE_DIR");
    let root = std::env::temp_dir().join(format!(
        "bone-rejected-config-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    unsafe { std::env::set_var("BONE_DIR", &root) };

    let store = crate::config::store::ConfigStore::new(crate::ext::ExtensionManager::unloaded())
        .expect("seed configuration");
    let provider =
        crate::llm::providers::create_provider_with_config("local", &store.providers_config())
            .unwrap();
    let (command_tx, mut command_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_, events_rx) = tokio::sync::broadcast::channel(8);
    let mut app = App::with_runtime_client(
        std::sync::Arc::from(provider),
        crate::config::UserConfig::default(),
        crate::config::custom::CustomConfigs::default(),
        command_tx,
        events_rx,
        None,
    )
    .unwrap();
    assert!(matches!(
        command_rx.try_recv().unwrap(),
        crate::runtime::RuntimeCommand::GetConfig
    ));
    app.config_view = config_view();
    app.config_view.snapshot.as_mut().unwrap().values =
        serde_json::json!({ "general": { "approval": "safe" } });
    app.approval_mode = crate::tools::ApprovalMode::Danger;
    app.user_config.approval_mode = crate::tools::ApprovalMode::Danger;
    app.pending_config
        .insert("request-1".into(), "general.approval".into());

    assert_eq!(
        app.reject_config_change(7, Some("request-1".into()))
            .as_deref(),
        Some("general.approval")
    );
    assert_eq!(app.approval_mode, crate::tools::ApprovalMode::Safe);
    assert_eq!(
        app.user_config.approval_mode,
        crate::tools::ApprovalMode::Safe
    );
    assert!(matches!(
        command_rx.try_recv().unwrap(),
        crate::runtime::RuntimeCommand::GetConfig
    ));

    drop(app);
    std::fs::remove_dir_all(root).ok();
    unsafe {
        match previous {
            Some(value) => std::env::set_var("BONE_DIR", value),
            None => std::env::remove_var("BONE_DIR"),
        }
    }
}

#[test]
fn pending_config_clears_only_for_matching_response() {
    let mut pending = std::collections::BTreeMap::from([
        ("request-1".to_string(), "general.approval".to_string()),
        ("request-2".to_string(), "ui.status_show_model".to_string()),
    ]);
    assert_eq!(
        take_pending_config(&mut pending, Some("other".into())),
        None
    );
    assert_eq!(pending.len(), 2);
    assert_eq!(
        take_pending_config(&mut pending, Some("request-2".into())).as_deref(),
        Some("ui.status_show_model")
    );
    assert_eq!(pending.len(), 1);
}

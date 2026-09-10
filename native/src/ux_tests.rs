use super::*;

#[test]
fn incognito_targets_selected_task_and_never_falls_back_to_another_connection() {
    let ctx = egui::Context::default();
    let mut app = DesktopApp::open(ctx.clone(), false, None);
    app.add_tab(Intent::New, &ctx);
    let (a_tx, mut a_rx) = mpsc::unbounded_channel();
    let (b_tx, mut b_rx) = mpsc::unbounded_channel();
    app.tabs[0].commands = a_tx;
    app.tabs[1].commands = b_tx;
    app.tabs[0].connected = true;
    app.tabs[1].connected = true;
    app.selected = 1;
    assert!(app.set_incognito(true));
    assert!(a_rx.try_recv().is_err());
    assert!(matches!(
        b_rx.try_recv(),
        Ok(Command::Send(RuntimeCommand::SetIncognito {
            enabled: true
        }))
    ));
    app.tabs[1].connected = false;
    assert!(!app.set_incognito(false));
    assert!(a_rx.try_recv().is_err());
}

#[test]
fn palette_app_actions_and_command_execution_preserve_the_draft() {
    let ctx = egui::Context::default();
    let mut app = DesktopApp::open(ctx.clone(), false, None);
    let id = app.tabs[0].id;
    app.tabs[0].composer = "keep this prompt".into();
    app.run_palette_action(id, "config", &ctx);
    assert!(app.show_config);
    assert_eq!(app.tabs[0].composer, "keep this prompt");
    app.show_config = false;
    app.run_palette_action(id, "custom-command", &ctx);
    assert!(app.command_input.is_some());
    let tab = &mut app.tabs[0];
    let (tx, mut rx) = mpsc::unbounded_channel();
    tab.commands = tx;
    tab.connected = true;
    tab.state.ready = true;
    assert!(tab.run_separate_command("custom-command", "arguments"));
    assert_eq!(tab.composer, "keep this prompt");
    assert!(
        matches!(rx.try_recv(), Ok(Command::Send(RuntimeCommand::RunCommand { name, input, .. })) if name == "custom-command" && input == "arguments")
    );
}

#[test]
fn model_selection_targets_origin_task_without_writing_provider_defaults() {
    let ctx = egui::Context::default();
    let mut app = DesktopApp::open(ctx.clone(), false, None);
    app.add_tab(Intent::New, &ctx);
    app.config = Some(tests::sample_config(
        7,
        "configured",
        &[("configured", "default-model")],
    ));
    let (tx, mut rx) = mpsc::unbounded_channel();
    app.tabs[1].commands = tx;
    app.tabs[1].connected = true;
    app.tabs[1].state.ready = true;
    app.tabs[1].host_api_version = 3;
    app.selected = 0;
    let id = app.tabs[1].id;
    assert!(app.choose_task_model(id, "configured", "custom-model"));
    assert!(
        matches!(rx.try_recv(), Ok(Command::Send(RuntimeCommand::SetConversationModel { provider_id, model })) if provider_id == "configured" && model == "custom-model")
    );
    assert_eq!(
        app.config.as_ref().unwrap().providers[0].model,
        "default-model"
    );
    app.tabs[1].host_api_version = 2;
    assert!(!app.choose_task_model(id, "configured", "custom-model"));
    assert!(rx.try_recv().is_err());
    assert!(app.choose_task_model(id, "configured", "default-model"));
    assert!(matches!(
        rx.try_recv(),
        Ok(Command::Send(RuntimeCommand::SwitchProvider { .. }))
    ));
}

#[test]
fn sidebar_search_filters_both_sections_and_rename_updates_open_title() {
    let ctx = egui::Context::default();
    let mut app = DesktopApp::open(ctx.clone(), false, None);
    app.tabs[0].conversation_id = Some(1);
    app.add_tab(Intent::Load(2), &ctx);
    app.conversations_loaded = true;
    app.conversations = vec![
        tests::sample_meta(1, "Match open"),
        tests::sample_meta(2, "Hide open"),
        tests::sample_meta(3, "Match recent"),
        tests::sample_meta(4, "Hide recent"),
    ];
    app.history_search = "match".into();
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| app.sidebar_lists(ui));
    let text = output
        .shapes
        .iter()
        .filter_map(|s| match &s.shape {
            egui::Shape::Text(t) => Some(t.galley.text()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    output.textures_delta.clear();
    assert!(
        text.contains("Match open") && text.contains("Match recent"),
        "{text}"
    );
    assert!(
        !text.contains("Hide open") && !text.contains("Hide recent"),
        "{text}"
    );
    app.apply_host_response(HostResponse::Conversations(vec![tests::sample_meta(
        1,
        "Renamed task",
    )]));
    assert_eq!(app.tabs[0].title(), "Renamed task");
}

#[test]
fn delete_closes_its_task_before_sending_the_durable_mutation() {
    let ctx = egui::Context::default();
    let mut app = DesktopApp::open(ctx.clone(), false, None);
    app.tabs[0].conversation_id = Some(1);
    app.tabs[0].connected = true;
    app.tabs[0].state.busy = true;
    let (tx, mut rx) = mpsc::unbounded_channel();
    app.tabs[0].commands = tx;
    app.add_tab(Intent::New, &ctx);
    app.tabs[1].connected = true;
    let (other_tx, mut other_rx) = mpsc::unbounded_channel();
    app.tabs[1].commands = other_tx;
    app.start_delete(1);
    assert!(rx.try_recv().is_err());
    app.commit_delete();
    assert!(matches!(
        rx.try_recv(),
        Ok(Command::Send(RuntimeCommand::Cancel))
    ));
    assert!(matches!(rx.try_recv(), Ok(Command::Disconnect)));
    app.poll_task_delete(&ctx);
    assert!(
        other_rx.try_recv().is_err(),
        "wait for disconnect acknowledgement"
    );
    app.tabs[0].handle_event(Event::Disconnected("Disconnected".into()));
    app.prune_closed();
    app.poll_task_delete(&ctx);
    assert!(matches!(
        other_rx.try_recv(),
        Ok(Command::Send(RuntimeCommand::HostRequest {
            request: HostRequest::ConversationDelete { id: 1, .. },
            ..
        }))
    ));
    assert!(app.pending_delete.is_none());
}

#[derive(Debug)]
struct TestDrop;
impl egui::DroppedFile for TestDrop {
    fn path(&self) -> &std::path::Path {
        std::path::Path::new("sample.png")
    }
    fn bytes(&self) -> Result<Vec<u8>, String> {
        Ok(b"fixture".to_vec())
    }
}

#[test]
fn shortcut_close_targets_the_focused_visible_pane() {
    let ctx = egui::Context::default();
    let mut app = DesktopApp::open(ctx.clone(), false, None);
    app.add_tab(Intent::New, &ctx);
    app.split_conversation(app.tabs[1].id, workspace::Axis::Horizontal, &ctx);
    app.tabs[1].composer = "right draft".into();
    app.apply_shortcut(egui::Key::W, &ctx);
    assert_eq!(app.close_target, Some(app.tabs[1].id));
    assert!(!app.tabs[0].closing);
}

#[test]
fn drops_attach_only_inside_the_destination_pane() {
    let ctx = egui::Context::default();
    let mut app = DesktopApp::open(ctx.clone(), false, None);
    app.add_tab(Intent::New, &ctx);
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
    app.last_pointer = Some(egui::pos2(650.0, 300.0));
    ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(screen),
            dropped_files: vec![std::sync::Arc::new(TestDrop)],
            ..Default::default()
        },
        |ui| {
            egui::Panel::right("test-right")
                .default_size(380.0)
                .show(ui, |ui| {
                    app.conversation_pane(1, ui);
                });
            app.conversation_pane(0, ui);
        },
    )
    .textures_delta
    .clear();
    assert!(app.tabs[0].attachments.is_empty());
    assert_eq!(app.tabs[1].attachments.len(), 1);
}

#[test]
fn remote_connections_require_a_loopback_tunnel_endpoint() {
    for address in [
        "example.com:7878",
        "192.168.1.10:7878",
        "0.0.0.0:7878",
        "[::]:7878",
        "localhost.evil:7878",
    ] {
        assert!(daemon::local_endpoint(address).is_err(), "{address}");
    }
    for address in ["127.0.0.1:7878", "localhost:17878", "[::1]:7878"] {
        assert!(daemon::local_endpoint(address).unwrap().ip().is_loopback());
    }
}

#[test]
fn closing_busy_or_draft_tabs_requires_confirmation() {
    let ctx = egui::Context::default();
    let mut app = DesktopApp::open(ctx, false, None);
    let (tx, mut rx) = mpsc::unbounded_channel();
    app.tabs[0].commands = tx;
    app.tabs[0].connected = true;
    app.tabs[0].state.busy = true;
    app.close_tab(0);
    assert_eq!(app.close_target, Some(app.tabs[0].id));
    assert!(!app.tabs[0].closing);
    assert!(rx.try_recv().is_err());
    app.close_target = None;
    app.tabs[0].state.busy = false;
    app.tabs[0].composer = "keep my draft".into();
    app.close_tab(0);
    assert!(app.close_target.is_some());
    assert_eq!(app.tabs[0].composer, "keep my draft");
    app.tabs[0].state.busy = true;
    app.finish_close_tab(0);
    assert!(matches!(
        rx.try_recv(),
        Ok(Command::Send(RuntimeCommand::Cancel))
    ));
    assert!(matches!(rx.try_recv(), Ok(Command::Disconnect)));
}

#[test]
fn window_manager_approval_keys_only_reach_the_focused_enabled_pane() {
    for focused in [0, 1] {
        for blocked in [false, true] {
            let ctx = egui::Context::default();
            let mut app = DesktopApp::open(ctx.clone(), false, None);
            app.add_tab(Intent::New, &ctx);
            app.split_conversation(app.tabs[1].id, workspace::Axis::Horizontal, &ctx);
            app.focus_conversation(app.tabs[focused].id, &ctx);
            let mut receivers = Vec::new();
            for (index, tab) in app.tabs.iter_mut().enumerate() {
                let (tx, rx) = mpsc::unbounded_channel();
                tab.commands = tx;
                receivers.push(rx);
                tab.connected = true;
                tab.state.ready = true;
                tab.state.busy = true;
                tab.state.approvals.push(state::Approval {
                    id: index as u64 + 1,
                    name: "shell".into(),
                    summary: format!("Approval for pane {index}"),
                    preview: None,
                    blocked: None,
                });
            }
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 800.0));
            for _ in 0..3 {
                ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(screen),
                        ..Default::default()
                    },
                    |ui| app.render_workspace(ui),
                )
                .drop_without_applying_deltas();
            }
            app.show_config = blocked;
            ctx.memory_mut(|memory| memory.stop_text_input());
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen),
                    events: vec![egui::Event::Key {
                        key: egui::Key::Enter,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    ..Default::default()
                },
                |ui| app.render_workspace(ui),
            )
            .drop_without_applying_deltas();
            for (index, rx) in receivers.iter_mut().enumerate() {
                if index == focused && !blocked {
                    assert!(matches!(rx.try_recv(),
                        Ok(Command::Send(RuntimeCommand::ApprovalReply {
                            id, outcome: CallOutcome::Approve,
                        })) if id == index as u64 + 1));
                }
                assert!(
                    rx.try_recv().is_err(),
                    "unexpected reply from pane {index}, blocked={blocked}"
                );
            }
        }
    }
}

#[test]
fn window_manager_large_pasted_draft_survives_layout_restore() {
    let ctx = egui::Context::default();
    let mut app = DesktopApp::open(ctx.clone(), false, None);
    let pasted = "a long pasted instruction\n".repeat(40);
    app.tabs[0].composer = "Please follow this:\n".into();
    app.tabs[0].insert_paste_placeholder(&pasted, usize::MAX);
    let expected = format!("Please follow this:\n{pasted}");
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("bone-draft-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("layout.txt");
    layout::save(&path, &app.current_layout()).unwrap();
    let restored = DesktopApp::open(ctx, false, Some(path));
    assert_eq!(restored.tabs[0].expanded_composer(), expected);
    assert!(restored.tabs[0].pastes.is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn window_manager_steering_and_queued_pastes_keep_their_contents() {
    let ctx = egui::Context::default();
    let mut app = DesktopApp::open(ctx, false, None);
    let tab = &mut app.tabs[0];
    let (tx, mut rx) = mpsc::unbounded_channel();
    tab.commands = tx;
    tab.connected = true;
    tab.state.ready = true;
    tab.state.busy = true;
    let queued = "queued instruction\n".repeat(50);
    tab.insert_paste_placeholder(&queued, 0);
    tab.enqueue_composer();
    assert_eq!(tab.queue[0], queued.trim());
    tab.clear_input();
    let steering = "correct the current task\n".repeat(50);
    tab.insert_paste_placeholder(&steering, 0);
    tab.steer_composer();
    assert!(matches!(rx.try_recv(),
        Ok(Command::Send(RuntimeCommand::Steer { text })) if text == steering.trim()));
    assert_eq!(
        tab.history.last().map(String::as_str),
        Some(steering.trim())
    );
    assert!(tab.pastes.is_empty());
    tab.state.busy = false;
    tab.drain_queue();
    assert!(matches!(rx.try_recv(),
        Ok(Command::Send(RuntimeCommand::SubmitPrompt { text, .. })) if text == queued.trim()));
    assert!(tab.queue.is_empty());
}

#[test]
fn window_manager_reconnect_keeps_queue_paused_until_resumed() {
    let ctx = egui::Context::default();
    let mut app = DesktopApp::open(ctx, false, None);
    let tab = &mut app.tabs[0];
    let (tx, mut rx) = mpsc::unbounded_channel();
    tab.commands = tx;
    tab.connected = true;
    tab.state.ready = true;
    tab.conversation_id = Some(7);
    tab.queue.extend(["first".into(), "second".into()]);
    tab.handle_event(Event::Disconnected("Connection lost".into()));
    assert!(!tab.resume_queue());
    tab.handle_event(Event::Connected);
    for id in [1, 7] {
        tab.handle_event(Event::Runtime(RuntimeEvent::ConversationLoaded {
            messages: Vec::new(),
            snapshot: bone_protocol::SessionSnapshot {
                conversation_id: Some(id),
                ..Default::default()
            },
            busy: false,
        }));
    }
    while let Ok(command) = rx.try_recv() {
        assert!(!matches!(
            command,
            Command::Send(RuntimeCommand::SubmitPrompt { .. })
        ));
    }
    assert!(tab.state.ready);
    assert!(tab.queue_paused);
    assert_eq!(tab.queue.len(), 2);
    tab.drain_queue();
    assert!(rx.try_recv().is_err());
    assert!(tab.resume_queue());
    tab.drain_queue();
    assert!(matches!(rx.try_recv(),
        Ok(Command::Send(RuntimeCommand::SubmitPrompt { text, .. })) if text == "first"));
    assert_eq!(tab.queue.front().map(String::as_str), Some("second"));
}

#[test]
fn window_manager_closing_a_paused_queue_requires_confirmation() {
    let ctx = egui::Context::default();
    let mut app = DesktopApp::open(ctx, false, None);
    app.tabs[0]
        .queue
        .push_back("recover this instruction".into());
    app.tabs[0].queue_paused = true;
    app.close_tab(0);
    assert_eq!(app.close_target, Some(app.tabs[0].id));
    assert!(!app.tabs[0].closing);
}

#[test]
fn window_manager_tab_status_prioritizes_attention_and_tracks_location() {
    let ctx = egui::Context::default();
    let mut app = DesktopApp::open(ctx.clone(), false, None);
    let id = app.tabs[0].id;
    assert_eq!(app.conversation_location(id), "Main window · Pane 1");
    let window = app.workspace.detach_tab(id).unwrap();
    assert_eq!(
        app.conversation_location(id),
        format!("Window {window} · Pane 1")
    );
    let tab = &mut app.tabs[0];
    tab.connected = true;
    tab.state.busy = true;
    assert_eq!(tab.navigation_status().0, "Running");
    tab.state.approvals.push(state::Approval {
        id: 1,
        name: "shell".into(),
        summary: "Confirm".into(),
        preview: None,
        blocked: None,
    });
    assert_eq!(tab.navigation_status().0, "Needs approval");
    tab.state.approvals.clear();
    tab.state.pending_key = Some(2);
    assert_eq!(tab.navigation_status().0, "Waiting for input");
    tab.state.last_error = Some("failed".into());
    assert_eq!(tab.navigation_status().0, "Error");
    tab.state.last_error = None;
    tab.state.pending_key = None;
    tab.state.busy = false;
    tab.has_new_output = true;
    assert_eq!(tab.navigation_status().0, "Unread output");
    tab.has_new_output = false;
    tab.composer = "draft".into();
    assert_eq!(tab.navigation_status().0, "Draft");
}

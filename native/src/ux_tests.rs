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

/// Regression: expanding a past tool call to reveal its reasoning while the
/// transcript is pinned to the bottom must not move the bottom "activity" panel
/// (the reported lurch was localized there, not to the transcript content). The
/// transcript itself *does* reflow on the click, which this also asserts so the
/// test can't pass vacuously.
#[test]
fn expanding_a_past_rows_reasoning_while_pinned_keeps_the_activity_panel_still() {
    fn text_rect(out: &egui::FullOutput, needle: &str) -> Option<egui::Rect> {
        out.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(t) if t.galley.text() == needle => {
                Some(t.visual_bounding_rect())
            }
            _ => None,
        })
    }
    fn text_pos(out: &egui::FullOutput, needle: &str) -> Option<egui::Pos2> {
        out.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(t) if t.galley.text().contains(needle) => Some(t.pos),
            _ => None,
        })
    }

    const TARGET: usize = 195;

    let ctx = egui::Context::default();
    let mut app = DesktopApp::open(ctx.clone(), false, None);
    let tab_id = app.tabs[0].id;
    {
        let tab = &mut app.tabs[0];
        tab.state.ready = true;
        tab.state.busy = true;
        tab.state.status = "working…".into();
        tab.state.live_reasoning = Some("thinking…".into());
        tab.state.processes = vec![bone_protocol::ProcessSnapshot {
            id: "p1".into(),
            command: "cargo test".into(),
            owner: "conversation".into(),
            running: true,
            state: bone_protocol::ProcessState::Running,
            started_at: 1_000,
            finished_at: None,
            stdout: "running 5 tests".into(),
            stderr: String::new(),
            exit_code: None,
            signal: None,
            error: None,
        }];
        for i in 0..200 {
            if i == TARGET {
                // Reasoning is revealed only inside the tool call that produced
                // it, so the target row is a tool row whose heading carries a
                // unique marker to locate and click.
                let row = tab.state.push_row("tool: shell", "SHELL_LOG");
                tab.state.toolcards[row] = Some(crate::state::ToolCard {
                    name: "shell".into(),
                    state: crate::state::ToolState::Done,
                    args: None,
                    label: Some("TARGETTOOL".into()),
                    show_result: Some(false),
                    eager: None,
                });
                tab.state.thinking[row] = Some("NEEDLE first step\nsecond step\nthird step".into());
            } else {
                tab.state.push_row(
                    "assistant",
                    format!("filler row {i} with enough words to fill one line"),
                );
            }
        }
    }

    let anchor_text = "TARGETTOOL";
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 600.0));
    let composer_id = egui::Id::new(("composer", tab_id));
    let run = |app: &mut DesktopApp, events: Vec<egui::Event>| {
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(screen),
                events,
                ..Default::default()
            },
            |ui| app.render_workspace(ui),
        )
    };

    // Settle the layout, then capture the panel rect, the target tool row's
    // heading anchor, and the heading itself (the disclosure to click). The
    // first rendered frame converges the bottom panel from an off-screen startup
    // rect, so the target row may not be visible until it settles; capture the
    // last frame that has both the anchor and the panel rect. (Output is dropped
    // before asserting so a failure cannot trip epaint's unapplied-deltas guard.)
    let mut panel_before = None;
    let mut anchor_before = None;
    let mut target = None;
    for _ in 0..6 {
        let out = run(&mut app, vec![]);
        let pinned = app.tabs[0].stick_to_bottom;
        let collapsed = text_pos(&out, "NEEDLE").is_none();
        let anchor = text_pos(&out, &anchor_text);
        let heading = text_rect(&out, "TARGETTOOL").map(|r| r.center());
        let panel = egui::PanelState::load(&ctx, composer_id).map(|s| s.outer_rect);
        out.drop_without_applying_deltas();
        assert!(pinned, "the transcript should be pinned to the bottom");
        assert!(
            collapsed,
            "the target row's reasoning should start collapsed"
        );
        if let (Some(anchor), Some(panel)) = (anchor, panel) {
            anchor_before = Some(anchor);
            panel_before = Some(panel);
        }
        if let Some(heading) = heading {
            target = Some(heading);
        }
    }

    let target = target.expect("the tool row heading should be painted for the target row");
    let panel_before = panel_before.expect("the activity panel should persist a rect");
    let anchor_before = anchor_before.expect("the target row should be visible");
    assert!(
        panel_before.max.y <= screen.max.y + 1.0,
        "the activity panel should have settled on-screen, got {panel_before:?}"
    );

    // Press and release in a single frame: the exact reported interaction.
    let moved = egui::Event::PointerMoved(target);
    let press = egui::Event::PointerButton {
        pos: target,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    };
    let release = egui::Event::PointerButton {
        pos: target,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    };

    let mut needle_seen = false;
    let mut anchor_after = None;
    for frame in 0..6 {
        let events = if frame == 0 {
            vec![moved.clone(), press.clone(), release.clone()]
        } else {
            Vec::new()
        };
        let out = run(&mut app, events);
        let panel = egui::PanelState::load(&ctx, composer_id).map(|s| s.outer_rect);
        let pinned = app.tabs[0].stick_to_bottom;
        let needle = text_pos(&out, "NEEDLE").is_some();
        let anchor = text_pos(&out, &anchor_text);
        out.drop_without_applying_deltas();
        assert_eq!(
            panel,
            Some(panel_before),
            "the activity panel moved on frame {frame} of the expand click"
        );
        assert!(pinned, "the transcript should stay pinned across the click");
        needle_seen |= needle;
        anchor_after = anchor;
    }

    assert!(
        needle_seen,
        "clicking the tool heading should expand the reasoning"
    );
    assert_ne!(
        anchor_after,
        Some(anchor_before),
        "expanding a row should reflow the transcript"
    );
}

/// Collapsing a past row's reasoning while the transcript is pinned to the
/// bottom must not jolt the rows below it: the pinned offset has to land on the
/// new bottom the same frame, so those rows hold their screen position. The bug
/// painted them one row-height too high for a single frame before snapping back.
#[test]
fn collapsing_a_past_rows_reasoning_while_pinned_keeps_the_rows_below_still() {
    fn text_rect(out: &egui::FullOutput, needle: &str) -> Option<egui::Rect> {
        out.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(t) if t.galley.text() == needle => {
                Some(t.visual_bounding_rect())
            }
            _ => None,
        })
    }
    fn text_pos(out: &egui::FullOutput, needle: &str) -> Option<egui::Pos2> {
        out.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(t) if t.galley.text().contains(needle) => Some(t.pos),
            _ => None,
        })
    }

    const TARGET: usize = 190;
    let ctx = egui::Context::default();
    let mut app = DesktopApp::open(ctx.clone(), false, None);
    {
        let tab = &mut app.tabs[0];
        tab.state.ready = true;
        for i in 0..200 {
            if i == TARGET {
                // Reasoning is revealed only inside the tool call that produced
                // it, so the target row is a tool row whose heading carries a
                // unique marker to locate and click.
                let row = tab.state.push_row("tool: shell", "SHELL_LOG");
                tab.state.toolcards[row] = Some(crate::state::ToolCard {
                    name: "shell".into(),
                    state: crate::state::ToolState::Done,
                    args: None,
                    label: Some("TARGETTOOL".into()),
                    show_result: Some(false),
                    eager: None,
                });
                tab.state.thinking[row] =
                    Some("reasoning for the tool call\nsecond line\nthird line".into());
            } else {
                tab.state.push_row(
                    "assistant",
                    format!("filler row {i} with enough words to fill one line"),
                );
            }
        }
    }
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 600.0));
    let run = |app: &mut DesktopApp, events: Vec<egui::Event>| {
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(screen),
                events,
                ..Default::default()
            },
            |ui| app.render_workspace(ui),
        )
    };
    let below = |out: &egui::FullOutput| -> Vec<(usize, Option<f32>)> {
        (TARGET + 1..200)
            .map(|i| (i, text_pos(out, &format!("filler row {i} ")).map(|p| p.y)))
            .collect()
    };
    // The target tool row's heading is both the click target and the anchor: its
    // y-position moves while the reasoning is toggled and must settle back.
    let target_pos = |out: &egui::FullOutput| text_rect(out, "TARGETTOOL").map(|r| r.center());
    let anchor = |out: &egui::FullOutput| target_pos(out).map(|p| p.y);
    let click = |pos: egui::Pos2| {
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    };

    // Settle, then record the target row's anchor and the anchors of every row
    // below it while collapsed. The first rendered frame converges the bottom
    // panel from an off-screen startup rect, so capture the last settled frame.
    // (`run` output is dropped before asserting so a failing frame cannot trip
    // epaint's unapplied-deltas guard.)
    let mut below_before = None;
    let mut anchor_before = None;
    let mut target = None;
    for _ in 0..8 {
        let out = run(&mut app, vec![]);
        let pinned = app.tabs[0].stick_to_bottom;
        let below_now = below(&out);
        let nearest = target_pos(&out);
        out.drop_without_applying_deltas();
        assert!(pinned, "the transcript should start pinned to the bottom");
        if below_now.iter().any(|(_, y)| y.is_some()) && nearest.is_some() {
            below_before = Some(below_now);
            anchor_before = nearest.map(|p| p.y);
        }
        if let Some(nearest) = nearest {
            target = Some(nearest);
        }
    }
    let target = target.expect("the tool row heading should be painted for the target row");
    let below_before = below_before.expect("the rows below the target should be laid out");
    let anchor_before = anchor_before.expect("the target row should be visible");

    // Expanding pulls the target row's heading up; the rows below must hold still.
    for f in 0..6 {
        let out = run(&mut app, if f == 0 { click(target) } else { vec![] });
        let pinned = app.tabs[0].stick_to_bottom;
        let below_now = below(&out);
        out.drop_without_applying_deltas();
        assert!(pinned, "the transcript should stay pinned while expanding");
        assert_eq!(
            below_now, below_before,
            "expanding must not move the rows below the target (frame {f})"
        );
    }

    // Expanding moved the target row's heading, so re-locate its affordance.
    let target2 = {
        let out = run(&mut app, vec![]);
        let nearest = target_pos(&out);
        out.drop_without_applying_deltas();
        nearest
    };
    let target2 = target2.expect("the tool row heading should remain after expanding");

    // Collapse: on every frame — including the click frame — the rows below the
    // target must hold their screen position. The bug painted them a row-height
    // too high for one frame before snapping back.
    let mut anchor_returned = false;
    for f in 0..6 {
        let out = run(&mut app, if f == 0 { click(target2) } else { vec![] });
        let pinned = app.tabs[0].stick_to_bottom;
        let below_now = below(&out);
        let anchor_now = anchor(&out);
        out.drop_without_applying_deltas();
        assert!(pinned, "the transcript should stay pinned while collapsing");
        assert_eq!(
            below_now, below_before,
            "collapsing a past row's reasoning jolted the rows below it on frame {f}"
        );
        if let Some(now) = anchor_now
            && (now - anchor_before).abs() < 1.0
        {
            anchor_returned = true;
        }
    }
    assert!(
        anchor_returned,
        "the target row should settle back to its collapsed position"
    );
}

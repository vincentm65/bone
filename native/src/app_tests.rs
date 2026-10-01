use super::*;

/// A connected, ready session whose outgoing commands land in the returned
/// receiver instead of a socket.
fn session() -> (Session, mpsc::UnboundedReceiver<Command>) {
    let ctx = egui::Context::default();
    let (local_tx, _local_rx) = std::sync::mpsc::channel();
    let mut session = Session::new(1, None, &ctx, local_tx);
    let (tx, rx) = mpsc::unbounded_channel();
    session.commands = tx;
    session.connected = true;
    session.state.ready = true;
    (session, rx)
}

/// An app whose active chat is connected and ready, with its commands captured.
fn app() -> (DesktopApp, egui::Context, mpsc::UnboundedReceiver<Command>) {
    let ctx = egui::Context::default();
    let mut app = DesktopApp::open(ctx.clone(), false);
    let (tx, rx) = mpsc::unbounded_channel();
    let session = app.session_mut();
    session.commands = tx;
    session.connected = true;
    session.state.ready = true;
    (app, ctx, rx)
}

#[test]
fn workspace_state_restores_open_conversations_and_selection() {
    let ctx = egui::Context::default();
    let mut app = DesktopApp::open(ctx, false);
    let expected = WorkspaceState {
        chats: vec![Some(17), Some(23)],
        active_chat: 1,
        sidebar_open: Some(false),
    };

    app.restore_workspace(expected.clone());

    assert_eq!(app.workspace_state(), expected);
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.session().conversation_id, Some(23));
}

#[test]
fn restoring_onto_the_first_tab_focuses_its_composer() {
    let ctx = egui::Context::default();
    let mut app = DesktopApp::open(ctx.clone(), false);
    ctx.memory_mut(|memory| memory.request_focus(egui::Id::new("elsewhere")));

    app.restore_workspace(WorkspaceState {
        chats: vec![Some(17), Some(23)],
        active_chat: 0,
        sidebar_open: None,
    });

    let active = app.active_chat;
    assert_eq!(
        ctx.memory(|memory| memory.focused()),
        Some(editor_id(active))
    );
}

#[test]
fn conversation_identity_ignores_default_replay_but_accepts_detachment() {
    let (mut session, _rx) = session();
    assert!(session.load_conversation(41));
    session.handle_event(Event::Runtime(RuntimeEvent::StateSnapshot {
        snapshot: bone_protocol::SessionSnapshot::default(),
    }));
    assert_eq!(session.conversation_id, Some(41));
    assert!(!session.state.ready);

    session.handle_event(Event::Runtime(RuntimeEvent::ConversationLoaded {
        messages: Vec::new(),
        snapshot: bone_protocol::SessionSnapshot {
            conversation_id: Some(41),
            ..Default::default()
        },
        busy: false,
    }));
    assert!(session.state.ready);
    assert!(
        session.handle_event(Event::Runtime(RuntimeEvent::StateSnapshot {
            snapshot: bone_protocol::SessionSnapshot {
                incognito: true,
                ..Default::default()
            },
        }))
    );
    assert_eq!(session.conversation_id, None);
}

fn sent(rx: &mut mpsc::UnboundedReceiver<Command>) -> Vec<RuntimeCommand> {
    std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(|command| match command {
            Command::Send(command) => Some(command),
            Command::Connect(_) => None,
        })
        .collect()
}

fn advertise(session: &mut Session, names: &[&str]) {
    session.state.frontend = Some(state::FrontendState {
        settings: serde_json::json!({}),
        commands: names
            .iter()
            .map(|name| ((*name).to_string(), String::new()))
            .collect(),
    });
}

fn run(session: &mut Session, input: &str) {
    session.composer = input.into();
    session.submit_composer();
}

fn key_event(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}

fn frame(app: &mut DesktopApp, ctx: &egui::Context, events: Vec<egui::Event>) {
    ctx.run_ui(
        egui::RawInput {
            events,
            ..Default::default()
        },
        |ui| app.handle_keys(ui),
    )
    .textures_delta
    .clear();
}

fn job_snapshot(id: &str) -> bone_protocol::JobSnapshot {
    bone_protocol::JobSnapshot {
        id: id.into(),
        agent: format!("agent-{id}"),
        task: format!("task-{id}"),
        title: String::new(),
        status: bone_protocol::JobStatus::Running,
        started_at: 0,
        token_sent: 0,
        token_received: 0,
        provider: "test".into(),
        activity: None,
        events: Vec::new(),
    }
}

fn process_snapshot(id: &str) -> bone_protocol::ProcessSnapshot {
    bone_protocol::ProcessSnapshot {
        id: id.into(),
        command: format!("command-{id}"),
        owner: "test".into(),
        running: true,
        state: bone_protocol::ProcessState::Running,
        started_at: 0,
        finished_at: None,
        stdout: String::new(),
        stderr: String::new(),
        exit_code: None,
        signal: None,
        error: None,
    }
}

#[test]
fn plain_arrows_recall_history_and_navigate_jobs() {
    let (mut app, ctx, _rx) = app();
    {
        let session = app.session_mut();
        session.history = vec!["oldest".into(), "newest".into()];
        session.state.jobs = vec![job_snapshot("first"), job_snapshot("second")];
        session.live_pane.active = Some("jobs".into());
    }
    let none = egui::Modifiers::NONE;

    frame(&mut app, &ctx, vec![key_event(egui::Key::ArrowUp, none)]);
    assert_eq!(app.session().composer, "newest");
    assert_eq!(app.session().history_index, Some(1));
    frame(&mut app, &ctx, vec![key_event(egui::Key::ArrowUp, none)]);
    assert_eq!(app.session().composer, "oldest");
    assert_eq!(app.session().history_index, Some(0));
    frame(&mut app, &ctx, vec![key_event(egui::Key::ArrowUp, none)]);
    assert_eq!(
        app.session().composer,
        "oldest",
        "history clamps at its oldest"
    );

    frame(&mut app, &ctx, vec![key_event(egui::Key::ArrowDown, none)]);
    assert_eq!(app.session().composer, "newest");
    frame(&mut app, &ctx, vec![key_event(egui::Key::ArrowDown, none)]);
    assert!(app.session().composer.is_empty());
    assert!(app.session().history_index.is_none());

    frame(&mut app, &ctx, vec![key_event(egui::Key::ArrowDown, none)]);
    assert_eq!(app.session().live_pane.job.as_deref(), Some("first"));
    assert!(app.session().live_pane.job_focused);
    frame(&mut app, &ctx, vec![key_event(egui::Key::ArrowDown, none)]);
    assert_eq!(app.session().live_pane.job.as_deref(), Some("second"));
    frame(&mut app, &ctx, vec![key_event(egui::Key::ArrowDown, none)]);
    assert_eq!(app.session().live_pane.job.as_deref(), Some("second"));
    frame(&mut app, &ctx, vec![key_event(egui::Key::ArrowUp, none)]);
    assert_eq!(app.session().live_pane.job.as_deref(), Some("first"));
    frame(&mut app, &ctx, vec![key_event(egui::Key::ArrowUp, none)]);
    assert!(!app.session().live_pane.job_focused);
    assert!(app.session().composer.is_empty());
    assert!(app.session().history_index.is_none());

    frame(&mut app, &ctx, vec![key_event(egui::Key::ArrowUp, none)]);
    assert_eq!(app.session().composer, "newest");
}

#[test]
fn plain_arrows_preserve_an_unsent_draft_without_history() {
    let (mut app, ctx, _rx) = app();
    {
        let session = app.session_mut();
        session.composer = "unsent draft".into();
        session.state.jobs = vec![job_snapshot("job")];
        session.live_pane.active = Some("jobs".into());
    }
    let none = egui::Modifiers::NONE;
    frame(&mut app, &ctx, vec![key_event(egui::Key::ArrowUp, none)]);
    frame(&mut app, &ctx, vec![key_event(egui::Key::ArrowDown, none)]);
    assert_eq!(app.session().composer, "unsent draft");
    assert!(app.session().history_index.is_none());
    assert!(!app.session().live_pane.job_focused);
    assert!(app.session().live_pane.job.is_none());
}

#[test]
fn process_arrows_match_job_navigation() {
    let (mut app, ctx, _rx) = app();
    {
        let session = app.session_mut();
        session.state.processes = vec![process_snapshot("first"), process_snapshot("second")];
        session.live_pane.active = Some("processes".into());
    }
    let none = egui::Modifiers::NONE;
    frame(&mut app, &ctx, vec![key_event(egui::Key::ArrowDown, none)]);
    assert_eq!(app.session().live_pane.process.as_deref(), Some("first"));
    assert!(app.session().live_pane.process_focused);
    frame(&mut app, &ctx, vec![key_event(egui::Key::ArrowDown, none)]);
    assert_eq!(app.session().live_pane.process.as_deref(), Some("second"));
    frame(&mut app, &ctx, vec![key_event(egui::Key::ArrowDown, none)]);
    assert_eq!(app.session().live_pane.process.as_deref(), Some("second"));
    frame(&mut app, &ctx, vec![key_event(egui::Key::ArrowUp, none)]);
    assert_eq!(app.session().live_pane.process.as_deref(), Some("first"));
    frame(&mut app, &ctx, vec![key_event(egui::Key::ArrowUp, none)]);
    assert!(!app.session().live_pane.process_focused);
    assert!(app.session().composer.is_empty());
}

#[test]
fn enter_opens_a_selected_job_or_process_only_for_trimmed_empty_input() {
    let (mut job_app, job_ctx, _rx) = app();
    {
        let session = job_app.session_mut();
        session.state.jobs = vec![job_snapshot("job")];
        session.live_pane.active = Some("jobs".into());
    }
    let none = egui::Modifiers::NONE;
    frame(
        &mut job_app,
        &job_ctx,
        vec![key_event(egui::Key::ArrowDown, none)],
    );
    job_app.session_mut().composer = "typed prompt".into();
    frame(
        &mut job_app,
        &job_ctx,
        vec![key_event(egui::Key::Enter, none)],
    );
    assert_eq!(job_app.tabs.len(), 1, "text keeps Enter in the composer");
    job_app.session_mut().composer = " \n\t".into();
    frame(
        &mut job_app,
        &job_ctx,
        vec![key_event(egui::Key::Enter, none)],
    );
    assert_eq!(
        job_app.tabs.len(),
        2,
        "trimmed-empty input opens the selected job"
    );

    let (mut process_app, process_ctx, _rx) = app();
    {
        let session = process_app.session_mut();
        session.state.processes = vec![process_snapshot("process")];
        session.live_pane.active = Some("processes".into());
    }
    frame(
        &mut process_app,
        &process_ctx,
        vec![key_event(egui::Key::ArrowDown, none)],
    );
    frame(
        &mut process_app,
        &process_ctx,
        vec![key_event(egui::Key::Enter, none)],
    );
    assert_eq!(
        process_app.tabs.len(),
        2,
        "Enter opens the selected process"
    );

    let (mut stale_app, stale_ctx, _rx) = app();
    {
        let session = stale_app.session_mut();
        session.state.jobs = vec![job_snapshot("real")];
        session.live_pane.active = Some("jobs".into());
        session.live_pane.job = Some("stale".into());
        session.live_pane.job_focused = true;
    }
    frame(
        &mut stale_app,
        &stale_ctx,
        vec![key_event(egui::Key::Enter, none)],
    );
    assert_eq!(stale_app.tabs.len(), 1, "stale selections never open");
}

#[test]
fn autocomplete_has_plain_arrow_precedence_over_agent_navigation() {
    let (mut app, ctx, _rx) = app();
    {
        let session = app.session_mut();
        session.composer = "/".into();
        session.state.jobs = vec![job_snapshot("job")];
        session.live_pane.active = Some("jobs".into());
    }
    render_frame(&mut app, &ctx, Vec::new());
    assert!(app.session().autocomplete_open());
    render_frame(
        &mut app,
        &ctx,
        vec![key_event(egui::Key::ArrowDown, egui::Modifiers::NONE)],
    );
    let session = app.session();
    assert_eq!(session.autocomplete.as_ref().unwrap().selected, 1);
    assert!(!session.live_pane.job_focused);
    assert!(session.history_index.is_none());
    assert_eq!(session.composer, "/");
}

#[test]
fn autocomplete_escape_stays_dismissed_until_the_draft_changes() {
    for busy in [false, true] {
        let (mut app, ctx, mut rx) = app();
        app.session_mut().composer = "/h".into();
        app.session_mut().state.busy = busy;
        render_frame(&mut app, &ctx, Vec::new());
        assert!(app.session().autocomplete_open());
        assert!(ctx.memory(|memory| memory.has_focus(editor_id(app.session().id))));

        render_frame(
            &mut app,
            &ctx,
            vec![key_event(egui::Key::Escape, egui::Modifiers::NONE)],
        );
        assert!(
            !app.session().autocomplete_open(),
            "Esc must close this frame"
        );
        for _ in 0..3 {
            render_frame(&mut app, &ctx, Vec::new());
            assert!(
                !app.session().autocomplete_open(),
                "idle refresh must not reopen"
            );
        }
        assert_eq!(app.session().composer, "/h");
        assert!(ctx.memory(|memory| memory.has_focus(editor_id(app.session().id))));
        assert!(
            !sent(&mut rx)
                .iter()
                .any(|command| matches!(command, RuntimeCommand::Cancel))
        );

        render_frame(&mut app, &ctx, vec![egui::Event::Text("e".into())]);
        assert_eq!(app.session().composer, "/he");
        assert!(
            app.session().autocomplete_open(),
            "editing reopens suggestions"
        );
    }
}

#[test]
fn autocomplete_tab_acceptance_stays_closed() {
    let (mut app, ctx, _rx) = app();
    app.session_mut().composer = "/he".into();
    render_frame(&mut app, &ctx, Vec::new());
    render_frame(
        &mut app,
        &ctx,
        vec![key_event(egui::Key::Tab, egui::Modifiers::NONE)],
    );
    assert_eq!(app.session().composer, "/help");
    render_frame(&mut app, &ctx, Vec::new());
    assert!(
        !app.session().autocomplete_open(),
        "Tab acceptance closes the list"
    );
}

#[test]
fn editing_clears_agent_focus_and_only_focused_lists_highlight_selection() {
    let (mut app, ctx, _rx) = app();
    {
        let session = app.session_mut();
        session.composer = "x".into();
        session.state.jobs = vec![job_snapshot("job")];
        session.live_pane.active = Some("jobs".into());
        session.live_pane.job = Some("job".into());
        session.live_pane.job_focused = true;
    }
    frame(&mut app, &ctx, vec![egui::Event::Text("x".into())]);
    assert!(!app.session().live_pane.job_focused);

    let (mut session, _rx) = session();
    session.state.jobs = vec![job_snapshot("job")];
    session.state.processes = vec![process_snapshot("process")];
    session.live_pane.job = Some("job".into());
    session.live_pane.process = Some("process".into());
    session.live_pane.job_focused = true;
    let theme = bone_render::theme::Theme::default();
    let pages = session.live_pane.pages(
        live_pane::Sources {
            view: &session.state.view,
            jobs: &session.state.jobs,
            processes: &session.state.processes,
            thinking: None,
            queue: &session.queue,
            approval: None,
        },
        &theme,
    );
    let line = |source: &str| {
        pages
            .iter()
            .find(|page| page.page.source == source)
            .unwrap()
            .page
            .content[0]
            .to_string()
    };
    assert!(line("jobs").starts_with(" › "));
    assert!(line("processes").starts_with("   "));

    session.live_pane.clear_focus();
    let pages = session.live_pane.pages(
        live_pane::Sources {
            view: &session.state.view,
            jobs: &session.state.jobs,
            processes: &session.state.processes,
            thinking: None,
            queue: &session.queue,
            approval: None,
        },
        &theme,
    );
    assert!(
        pages
            .iter()
            .find(|page| page.page.source == "jobs")
            .unwrap()
            .page
            .content[0]
            .to_string()
            .starts_with("   ")
    );
    assert!(
        pages
            .iter()
            .find(|page| page.page.source == "processes")
            .unwrap()
            .page
            .content[0]
            .to_string()
            .starts_with("   ")
    );
}

#[test]
fn config_and_provider_menus_run_the_lua_config_command() {
    let (mut session, mut rx) = session();
    advertise(&mut session, &["config"]);
    run(&mut session, "/config");
    run(&mut session, "/provider");
    let names: Vec<_> = sent(&mut rx)
        .into_iter()
        .filter_map(|command| match command {
            RuntimeCommand::RunCommand { name, input, .. } => Some((name, input)),
            _ => None,
        })
        .collect();
    assert_eq!(
        names,
        vec![
            ("config".to_string(), String::new()),
            ("config".to_string(), "providers".to_string())
        ]
    );
}

#[test]
fn provider_switch_is_idempotent_and_reports_only_the_request() {
    let (mut session, mut rx) = session();
    session.state.snapshot.provider_id = "openai".into();

    run(&mut session, "/provider openai");
    assert!(sent(&mut rx).is_empty(), "current provider needs no switch");
    assert_eq!(
        session.state.rows.last().map(|(_, text)| text.as_str()),
        Some("Already using provider openai.")
    );

    run(&mut session, "/provider anthropic");
    assert!(matches!(
        sent(&mut rx).as_slice(),
        [RuntimeCommand::SwitchProvider { provider_id }] if provider_id == "anthropic"
    ));
    assert_eq!(
        session.state.rows.last().map(|(_, text)| text.as_str()),
        Some("Requested provider anthropic.")
    );

    session.state.snapshot.provider_id = "anthropic".into();
    let reply = session.apply_command_action(CommandAction {
        config_action: Some(ConfigAction::SwitchProvider {
            id: "anthropic".into(),
        }),
        ..Default::default()
    });
    assert_eq!(reply.as_deref(), Some("Already using provider anthropic."));
    assert!(
        sent(&mut rx).is_empty(),
        "config action also avoids a duplicate"
    );
}

#[test]
fn full_screen_commands_open_page_tabs() {
    let (mut app, _ctx, mut rx) = app();
    for command in ["/stats", "/setup", "/catalog"] {
        run(app.session_mut(), command);
    }
    app.drain_page_requests();
    let titles: Vec<String> = app.tabs.iter().map(|tab| app.tab_title(tab)).collect();
    assert_eq!(titles[1..], ["stats", "setup", "catalog"]);
    assert_eq!(app.selected, 3, "the newest page is selected");
    let requests: Vec<_> = sent(&mut rx)
        .into_iter()
        .filter_map(|command| match command {
            RuntimeCommand::HostRequest { request, .. } => Some(request),
            _ => None,
        })
        .collect();
    assert!(matches!(requests[0], HostRequest::Stats { range: None }));
    assert!(matches!(requests[1], HostRequest::Setup));
    assert!(matches!(
        requests[2],
        HostRequest::Catalog { refresh: true }
    ));
}

#[test]
fn page_keys_reach_the_page_and_esc_closes_its_tab() {
    let (mut app, ctx, _rx) = app();
    run(app.session_mut(), "/stats");
    app.drain_page_requests();
    assert_eq!(app.tabs.len(), 2);
    frame(
        &mut app,
        &ctx,
        vec![key_event(egui::Key::Escape, egui::Modifiers::NONE)],
    );
    assert_eq!(app.tabs.len(), 1, "Esc closes the stats page");
    assert_eq!(app.selected, 0);
}

#[test]
fn ctrl_t_and_ctrl_w_manage_chat_tabs_and_the_last_chat_stays() {
    let (mut app, ctx, _rx) = app();
    frame(
        &mut app,
        &ctx,
        vec![key_event(egui::Key::T, egui::Modifiers::COMMAND)],
    );
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.active_chat, app.tabs[1].id);
    frame(
        &mut app,
        &ctx,
        vec![key_event(egui::Key::W, egui::Modifiers::COMMAND)],
    );
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.active_chat, app.tabs[0].id);
    frame(
        &mut app,
        &ctx,
        vec![key_event(egui::Key::W, egui::Modifiers::COMMAND)],
    );
    assert_eq!(app.tabs.len(), 1, "the last chat is never closed");
}

fn with_approval(app: &mut DesktopApp) {
    let session = app.session_mut();
    session.state.approvals.push(state::Approval {
        id: 9,
        call: bone_protocol::ToolCall {
            id: "call".into(),
            name: "shell".into(),
            arguments: serde_json::json!({"command": "ls -la"}),
        },
        blocked: None,
    });
    session.sync_prompt();
}

#[test]
fn approval_advise_sends_the_typed_advice() {
    let (mut app, ctx, mut rx) = app();
    with_approval(&mut app);
    let none = egui::Modifiers::NONE;
    frame(&mut app, &ctx, vec![key_event(egui::Key::ArrowDown, none)]);
    frame(&mut app, &ctx, vec![key_event(egui::Key::Enter, none)]);
    assert!(
        app.session().advising,
        "Enter on Advise starts advice entry"
    );
    app.session_mut().composer = "use ls -l instead".into();
    frame(&mut app, &ctx, vec![key_event(egui::Key::Enter, none)]);
    assert!(matches!(
        sent(&mut rx).as_slice(),
        [RuntimeCommand::ApprovalReply { id: 9, outcome: CallOutcome::Blocked(reply) }]
            if reply.ends_with("User advice: use ls -l instead")
    ));
}

#[test]
fn approval_cancel_denies_and_stops_the_turn() {
    let (mut app, ctx, mut rx) = app();
    with_approval(&mut app);
    frame(
        &mut app,
        &ctx,
        vec![key_event(egui::Key::Escape, egui::Modifiers::NONE)],
    );
    let commands = sent(&mut rx);
    assert!(matches!(commands[0], RuntimeCommand::Cancel));
    assert!(matches!(
        commands[1],
        RuntimeCommand::ApprovalReply {
            id: 9,
            outcome: CallOutcome::Denied
        }
    ));
}

#[test]
fn queue_list_keys_reorder_send_next_and_remove() {
    let (mut app, ctx, _rx) = app();
    let session = app.session_mut();
    session.queue = ["a".to_string(), "b".to_string(), "c".to_string()].into();
    session.live_pane.active = Some("queue".into());
    let theme = bone_render::theme::Theme::default();
    let session = app.session();
    let pages = session.live_pane.pages(
        live_pane::Sources {
            view: &session.state.view,
            jobs: &[],
            processes: &[],
            thinking: None,
            queue: &session.queue,
            approval: None,
        },
        &theme,
    );
    app.session_mut().live_pane.sync(&pages, &[], &[]);
    let none = egui::Modifiers::NONE;
    frame(&mut app, &ctx, vec![key_event(egui::Key::ArrowDown, none)]);
    frame(
        &mut app,
        &ctx,
        vec![key_event(egui::Key::ArrowDown, egui::Modifiers::SHIFT)],
    );
    assert_eq!(app.session().queue, ["a", "c", "b"]);
    frame(&mut app, &ctx, vec![key_event(egui::Key::Enter, none)]);
    assert_eq!(app.session().queue, ["b", "a", "c"], "Enter sends it next");
    frame(&mut app, &ctx, vec![key_event(egui::Key::Delete, none)]);
    assert_eq!(app.session().queue, ["a", "c"]);
}

#[test]
fn catalog_install_fetches_a_snapshot_then_applies() {
    let (mut app, _ctx, mut rx) = app();
    run(app.session_mut(), "/catalog install weather");
    app.drain_page_requests();
    let commands: [RuntimeCommand; 1] = sent(&mut rx).try_into().unwrap();
    let [
        RuntimeCommand::HostRequest {
            request_id,
            request,
        },
    ] = commands
    else {
        panic!("expected one host request");
    };
    assert!(matches!(request, HostRequest::Catalog { refresh: true }));
    let chat = app.active_chat;
    assert!(app.catalog_response(
        chat,
        request_id,
        HostResponse::Catalog(bone_protocol::CatalogSnapshot {
            revision: "r7".into(),
            items: Vec::new(),
        }),
    ));
    assert!(matches!(
        sent(&mut rx).as_slice(),
        [RuntimeCommand::HostRequest {
            request: HostRequest::CatalogApply { expected_revision, actions },
            ..
        }] if expected_revision == "r7" && actions[0].name == "weather"
    ));
}

#[test]
fn busy_enter_queues_and_idle_drains_in_order() {
    let (mut session, mut rx) = session();
    session.state.busy = true;
    session.composer = "second".into();
    session.enqueue_composer();
    assert_eq!(session.queue.len(), 1);
    assert!(session.composer.is_empty());
    session.state.busy = false;
    session.drain_queue();
    assert!(matches!(
        sent(&mut rx).as_slice(),
        [RuntimeCommand::SubmitPrompt { text, .. }] if text == "second"
    ));
}

#[test]
fn queued_prompts_wait_for_an_explicit_send_after_a_disconnect() {
    let (mut session, mut rx) = session();
    session.state.busy = true;
    session.composer = "queued".into();
    session.enqueue_composer();
    session.handle_event(Event::Disconnected("lost link".into()));
    session.connected = true;
    session.state.ready = true;
    session.drain_queue();
    assert!(sent(&mut rx).is_empty());
    assert_eq!(session.queue.len(), 1);
    session.submit_composer_in_order();
    assert!(matches!(
        sent(&mut rx).as_slice(),
        [RuntimeCommand::SubmitPrompt { text, .. }] if text == "queued"
    ));
}

#[test]
fn pending_key_requests_receive_escape_and_tab() {
    let (mut app, ctx, mut rx) = app();
    let session = app.session_mut();
    session.state.busy = true;
    session.state.pending_key = Some(7);
    frame(
        &mut app,
        &ctx,
        vec![key_event(egui::Key::Escape, egui::Modifiers::NONE)],
    );
    assert!(
        matches!(
            sent(&mut rx).as_slice(),
            [RuntimeCommand::KeyReply { id: 7, .. }]
        ),
        "Esc goes to the menu, not Cancel"
    );
    app.session_mut().state.pending_key = Some(8);
    frame(
        &mut app,
        &ctx,
        vec![key_event(egui::Key::Tab, egui::Modifiers::NONE)],
    );
    assert!(matches!(
        sent(&mut rx).as_slice(),
        [RuntimeCommand::KeyReply { id: 8, .. }]
    ));
}

#[test]
fn escape_cancels_a_running_turn() {
    let (mut app, ctx, mut rx) = app();
    app.session_mut().state.busy = true;
    frame(
        &mut app,
        &ctx,
        vec![key_event(egui::Key::Escape, egui::Modifiers::NONE)],
    );
    assert!(matches!(sent(&mut rx).as_slice(), [RuntimeCommand::Cancel]));
}

/// Top edge of the first painted text containing `needle`.
fn text_top(output: &egui::FullOutput, needle: &str) -> Option<f32> {
    fn visit(shape: &egui::Shape, needle: &str) -> Option<f32> {
        match shape {
            egui::Shape::Text(text) if text.galley.job.text.contains(needle) => Some(text.pos.y),
            egui::Shape::Vec(shapes) => shapes.iter().find_map(|shape| visit(shape, needle)),
            _ => None,
        }
    }
    output
        .shapes
        .iter()
        .find_map(|shape| visit(&shape.shape, needle))
}

/// Where the first painted text containing `needle` starts.
fn text_pos(output: &egui::FullOutput, needle: &str) -> Option<egui::Pos2> {
    fn visit(shape: &egui::Shape, needle: &str) -> Option<egui::Pos2> {
        match shape {
            egui::Shape::Text(text) if text.galley.job.text.contains(needle) => Some(text.pos),
            egui::Shape::Vec(shapes) => shapes.iter().find_map(|shape| visit(shape, needle)),
            _ => None,
        }
    }
    output
        .shapes
        .iter()
        .find_map(|shape| visit(&shape.shape, needle))
}

#[test]
fn regions_stack_like_the_tui() {
    let ctx = egui::Context::default();
    let mut app = DesktopApp::open(ctx.clone(), true);
    let mut output: Option<egui::FullOutput> = None;
    for _ in 0..3 {
        if let Some(previous) = output.as_mut() {
            previous.textures_delta.clear();
        }
        output = Some(ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 700.0),
                )),
                ..Default::default()
            },
            |ui| app.render(ui),
        ));
    }
    let mut output = output.unwrap();
    output.textures_delta.clear();
    let transcript = text_top(&output, "Show me a Markdown sample").expect("transcript");
    let input = text_top(&output, "> ").expect("input prefix");
    let live = text_top(&output, "Build the live pane").expect("live pane");
    let status = text_top(&output, "demo-model").expect("status bar");
    assert!(transcript < input, "{transcript} < {input}");
    assert!(input < live, "{input} < {live}");
    assert!(live < status, "{live} < {status}");
}

#[test]
fn pending_attachment_keeps_last_theme_until_frontend_state_arrives() {
    let ctx = egui::Context::default();
    let mut app = DesktopApp::open(ctx.clone(), true);
    let theme = serde_json::json!({
        "name": "configured",
        "palette": {
            "bg": "#123456",
            "fg": "#f0e0d0",
            "accent": "#abcdef"
        }
    });

    app.session_mut().state.theme = Some(theme.clone());
    render_frame(&mut app, &ctx, Vec::new());
    let configured_fill = ctx.style_of(ctx.theme()).visuals.panel_fill;
    assert_eq!(configured_fill, egui::Color32::from_rgb(0x12, 0x34, 0x56));

    // Loading another conversation clears the per-session snapshot while the
    // renderer still has the daemon-global theme it previously applied.
    app.session_mut().state.reset(Some(42));
    render_frame(&mut app, &ctx, Vec::new());
    assert_eq!(
        ctx.style_of(ctx.theme()).visuals.panel_fill,
        configured_fill
    );
    assert_eq!(app.applied_theme, Some(theme.clone()));

    let _ = app.session_mut().state.reduce(RuntimeEvent::FrontendState {
        banner: String::new(),
        settings: serde_json::json!({ "theme": theme }),
        commands: Vec::new(),
        tool_defs: Vec::new(),
        tool_display: serde_json::Value::Null,
        subagents: Vec::new(),
        host_api_version: 0,
        catalog_updates: 0,
        cwd: None,
    });
    render_frame(&mut app, &ctx, Vec::new());
    assert_eq!(
        ctx.style_of(ctx.theme()).visuals.panel_fill,
        configured_fill
    );
}

fn render_frame(
    app: &mut DesktopApp,
    ctx: &egui::Context,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    let mut output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 700.0),
            )),
            events,
            ..Default::default()
        },
        |ui| app.render(ui),
    );
    output.textures_delta.clear();
    output
}

#[test]
fn sidebar_rows_show_a_timestamp_under_each_title_and_open_on_click() {
    let ctx = egui::Context::default();
    let mut app = DesktopApp::open(ctx.clone(), true);
    let meta = |id: i64, title: &str| ConversationMeta {
        id,
        title: title.into(),
        full_title: String::new(),
        updated_at: "2020-01-02T14:22:00Z".into(),
        updated_at_local: "2020-01-02T14:22:00".into(),
        message_count: 12,
        provider: "openai".into(),
        model: "gpt".into(),
        token_count: 4_210,
        status: bone_protocol::ConversationStatus::Completed,
    };
    app.conversations = vec![meta(41, "first chat"), meta(42, "second chat")];
    let mut output = render_frame(&mut app, &ctx, Vec::new());
    for _ in 0..2 {
        output = render_frame(&mut app, &ctx, Vec::new());
    }

    let first = text_top(&output, "first chat").expect("first title");
    let second = text_top(&output, "second chat").expect("second title");
    let stamp = text_top(&output, "2020-01-02 14:22 · 12 msgs · 4.2k tok").expect("stamp");
    assert!(
        first < stamp && stamp < second,
        "{first} < {stamp} < {second}"
    );

    let click = egui::pos2(40.0, second + 4.0);
    render_frame(&mut app, &ctx, vec![egui::Event::PointerMoved(click)]);
    let button = |pressed| egui::Event::PointerButton {
        pos: click,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    render_frame(&mut app, &ctx, vec![button(true), button(false)]);
    let opened = app.tabs.iter().any(
        |tab| matches!(&tab.kind, TabKind::Chat(session) if session.conversation_id == Some(42)),
    );
    assert!(opened, "clicking a row opens that conversation");
}

#[test]
fn sidebar_rename_and_delete_send_host_mutations() {
    let meta = |id: i64, title: &str| ConversationMeta {
        id,
        title: title.into(),
        full_title: format!("{title} full"),
        updated_at: "2020-01-02T14:22:00Z".into(),
        updated_at_local: "2020-01-02T14:22:00".into(),
        message_count: 1,
        provider: "openai".into(),
        model: "gpt".into(),
        token_count: 0,
        status: bone_protocol::ConversationStatus::Completed,
    };

    let (mut rename_app, _ctx, mut rx) = app();
    rename_app.conversations = vec![meta(41, "old")];
    rename_app.start_rename(41);
    assert_eq!(rename_app.rename_field, "old full");
    rename_app.rename_field = "renamed".into();
    rename_app.commit_rename();
    let requests = sent(&mut rx);
    assert_eq!(requests.len(), 1);
    match &requests[0] {
        RuntimeCommand::HostRequest {
            request: HostRequest::ConversationRename { id, title, .. },
            ..
        } => {
            assert_eq!(*id, 41);
            assert_eq!(title, "renamed");
        }
        other => panic!("unexpected request: {other:?}"),
    }

    let (mut app, _ctx, mut rx) = app();
    app.conversations = vec![meta(41, "old")];
    app.start_delete(41);
    app.commit_delete();
    let requests = sent(&mut rx);
    assert_eq!(requests.len(), 1);
    assert!(matches!(
        &requests[0],
        RuntimeCommand::HostRequest {
            request: HostRequest::ConversationDelete { id: 41, .. },
            ..
        }
    ));
}

#[test]
fn deleting_only_empty_conversation_waits_for_a_replacement_chat() {
    let (mut app, _ctx, mut rx) = app();
    app.conversations = vec![ConversationMeta {
        id: 41,
        title: String::new(),
        full_title: String::new(),
        updated_at: "2020-01-02T14:22:00Z".into(),
        updated_at_local: "2020-01-02T14:22:00".into(),
        message_count: 0,
        provider: "openai".into(),
        model: "gpt".into(),
        token_count: 0,
        status: bone_protocol::ConversationStatus::Completed,
    }];
    app.session_mut().conversation_id = Some(41);
    app.start_delete(41);
    app.commit_delete();

    assert_eq!(app.pending_delete, Some(41));
    assert_eq!(
        app.tabs.len(),
        1,
        "a replacement chat tab keeps the UI usable"
    );
    assert!(app.session().conversation_id.is_none());
    assert!(
        sent(&mut rx).is_empty(),
        "deletion does not start a new conversation"
    );

    app.poll_pending_delete();
    assert!(
        sent(&mut rx).is_empty(),
        "deletion waits for the replacement connection"
    );

    let (replacement_tx, mut replacement_rx) = mpsc::unbounded_channel();
    app.session_mut().commands = replacement_tx;
    app.session_mut().connected = true;
    app.poll_pending_delete();
    assert!(app.pending_delete.is_none());
    assert!(matches!(
        sent(&mut replacement_rx).as_slice(),
        [RuntimeCommand::HostRequest {
            request: HostRequest::ConversationDelete { id: 41, .. },
            ..
        }]
    ));
}

#[test]
fn a_turn_finishing_unseen_marks_the_chat_unread_until_viewed() {
    let (mut app, ctx, _rx) = app();
    let drain = |app: &mut DesktopApp, focused: bool| {
        ctx.run_ui(
            egui::RawInput {
                focused,
                ..Default::default()
            },
            |ui| app.drain(ui.ctx()),
        )
        .textures_delta
        .clear();
    };
    app.session_mut().state.busy = true;
    drain(&mut app, true);
    app.session_mut().state.busy = false;
    drain(&mut app, true);
    assert!(
        !app.session().unread,
        "a turn finishing in view is already read"
    );

    app.session_mut().state.busy = true;
    drain(&mut app, false);
    app.session_mut().state.busy = false;
    drain(&mut app, false);
    assert!(
        app.session().unread,
        "finished while the window was unfocused"
    );
    drain(&mut app, false);
    assert!(app.session().unread, "stays unread until viewed");
    drain(&mut app, true);
    assert!(!app.session().unread, "viewing the tab marks it read");
}

fn render_at(
    app: &mut DesktopApp,
    ctx: &egui::Context,
    width: f32,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    let mut output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, 700.0),
            )),
            events,
            ..Default::default()
        },
        |ui| app.render(ui),
    );
    output.textures_delta.clear();
    output
}

/// Tap like a finger would: move to the spot, press, release, settle.
fn tap_at(
    app: &mut DesktopApp,
    ctx: &egui::Context,
    width: f32,
    at: egui::Pos2,
) -> egui::FullOutput {
    let press = |pressed| egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    render_at(app, ctx, width, vec![egui::Event::PointerMoved(at)]);
    render_at(app, ctx, width, vec![press(true), press(false)]);
    render_at(app, ctx, width, Vec::new())
}

/// Registers a touch device with egui, so later frames take the phone paths.
fn touch_device() -> Vec<egui::Event> {
    let at = egui::pos2(2.0, 2.0);
    let touch = |phase| egui::Event::Touch {
        device_id: egui::TouchDeviceId(1),
        id: egui::TouchId(1),
        phase,
        pos: at,
        force: None,
    };
    vec![touch(egui::TouchPhase::Start), touch(egui::TouchPhase::End)]
}

/// A finger tap: press, a little drift, release. Three frames, the way real
/// touch arrives, so egui sees the click a finger expects.
fn touch_tap_at(
    app: &mut DesktopApp,
    ctx: &egui::Context,
    width: f32,
    at: egui::Pos2,
    drift: egui::Vec2,
) {
    let button = |pos: egui::Pos2, pressed: bool| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    let moved = at + drift;
    render_at(app, ctx, width, vec![egui::Event::PointerMoved(at)]);
    render_at(app, ctx, width, vec![button(at, true)]);
    render_at(app, ctx, width, vec![egui::Event::PointerMoved(moved)]);
    render_at(app, ctx, width, vec![button(moved, false)]);
    render_at(app, ctx, width, Vec::new());
}

/// Names the tab at `index` directly, so a title cannot land on the wrong tab
/// while the app is still settling which tab a shortcut opened.
fn title_tab_at(app: &mut DesktopApp, index: usize, title: &str) {
    if let TabKind::Chat(session) = &mut app.tabs[index].kind {
        session
            .state
            .rows
            .push(("user".to_owned(), title.to_owned()));
    }
}

fn with_one_conversation(app: &mut DesktopApp) {
    app.conversations = vec![ConversationMeta {
        id: 41,
        title: "sidebar chat".into(),
        full_title: String::new(),
        updated_at: "2020-01-02T14:22:00Z".into(),
        updated_at_local: String::new(),
        message_count: 1,
        provider: "p".into(),
        model: "m".into(),
        token_count: 0,
        status: Default::default(),
    }];
}

#[test]
fn new_chat_refreshes_phone_sidebar_during_a_turn_and_can_be_reopened() {
    let (mut app, ctx, mut rx) = app();
    let (events_tx, events_rx) = mpsc::channel(16);
    app.session_mut().events = events_rx;
    app.conversations_stale = false;
    render_at(&mut app, &ctx, 400.0, Vec::new());
    sent(&mut rx);

    run(app.session_mut(), "new sidebar chat");
    assert!(app.session().state.busy);
    assert!(matches!(
        sent(&mut rx).as_slice(),
        [RuntimeCommand::SubmitPrompt { .. }]
    ));
    events_tx
        .try_send(Event::Runtime(RuntimeEvent::StateSnapshot {
            snapshot: bone_protocol::SessionSnapshot {
                conversation_id: Some(41),
                ..Default::default()
            },
        }))
        .unwrap();
    render_at(&mut app, &ctx, 400.0, Vec::new());
    assert_eq!(app.workspace_state().chats, vec![Some(41)]);
    let request_id = app
        .conversations_request
        .expect("history refresh while busy")
        .1;
    assert!(sent(&mut rx).iter().any(|command| matches!(
        command,
        RuntimeCommand::HostRequest {
            request: HostRequest::Conversations { .. },
            ..
        }
    )));

    with_one_conversation(&mut app);
    app.conversations[0].title = "new sidebar chat".into();
    let conversations = std::mem::take(&mut app.conversations);
    events_tx
        .try_send(Event::Runtime(RuntimeEvent::HostResponse {
            request_id,
            response: HostResponse::Conversations(conversations),
        }))
        .unwrap();
    app.sidebar_open = Some(true);
    let output = render_at(&mut app, &ctx, 400.0, Vec::new());
    assert!(text_top(&output, "new sidebar chat").is_some());
    assert!(app.session().state.busy, "no turn completion needed");

    app.close_tab(0);
    events_tx
        .try_send(Event::Runtime(RuntimeEvent::StateSnapshot {
            snapshot: bone_protocol::SessionSnapshot {
                conversation_id: Some(41),
                ..Default::default()
            },
        }))
        .unwrap();
    events_tx
        .try_send(Event::Runtime(RuntimeEvent::ConversationLoaded {
            messages: Vec::new(),
            snapshot: bone_protocol::SessionSnapshot::default(),
            busy: false,
        }))
        .unwrap();
    let output = render_at(&mut app, &ctx, 400.0, Vec::new());
    assert!(
        app.session().conversation_id.is_none(),
        "closed chat must detach"
    );
    let at = text_pos(&output, "new sidebar chat").expect("saved row stays after close");
    tap_at(&mut app, &ctx, 400.0, at + egui::vec2(4.0, 4.0));
    assert!(
        sent(&mut rx)
            .iter()
            .any(|command| matches!(command, RuntimeCommand::LoadConversation { id: 41, .. }))
    );
    assert_eq!(app.session().conversation_id, Some(41));
}

#[test]
fn ctrl_b_collapses_and_restores_the_sidebar_on_wide_windows() {
    let ctx = egui::Context::default();
    let mut app = DesktopApp::open(ctx.clone(), true);
    with_one_conversation(&mut app);
    render_at(&mut app, &ctx, 1000.0, Vec::new());
    let output = render_at(&mut app, &ctx, 1000.0, Vec::new());
    assert!(
        text_top(&output, "sidebar chat").is_some(),
        "wide windows start with the sidebar"
    );
    let tab = text_pos(&output, "Show me a Markdown sample").expect("chat tab");
    assert!(
        tab.x >= SIDEBAR_WIDTH,
        "tab strip starts after the sidebar: {tab:?}"
    );

    let ctrl_b = key_event(egui::Key::B, egui::Modifiers::COMMAND);
    render_at(&mut app, &ctx, 1000.0, vec![ctrl_b.clone()]);
    let output = render_at(&mut app, &ctx, 1000.0, Vec::new());
    assert!(
        text_top(&output, "sidebar chat").is_none(),
        "Ctrl+B hides it"
    );
    assert!(
        text_top(&output, "Show me a Markdown sample").is_some(),
        "the chat stays"
    );

    render_at(&mut app, &ctx, 1000.0, vec![ctrl_b]);
    let output = render_at(&mut app, &ctx, 1000.0, Vec::new());
    assert!(
        text_top(&output, "sidebar chat").is_some(),
        "Ctrl+B shows it again"
    );
}

#[test]
fn narrow_windows_start_collapsed_and_open_the_sidebar_full_width() {
    let ctx = egui::Context::default();
    let mut app = DesktopApp::open(ctx.clone(), true);
    with_one_conversation(&mut app);
    render_at(&mut app, &ctx, 400.0, Vec::new());
    let output = render_at(&mut app, &ctx, 400.0, Vec::new());
    assert!(
        text_top(&output, "sidebar chat").is_none(),
        "phones start with the chat"
    );
    assert!(text_top(&output, "Show me a Markdown sample").is_some());

    render_at(
        &mut app,
        &ctx,
        400.0,
        vec![key_event(egui::Key::B, egui::Modifiers::COMMAND)],
    );
    let output = render_at(&mut app, &ctx, 400.0, Vec::new());
    assert!(
        text_top(&output, "sidebar chat").is_some(),
        "opened as a drawer"
    );
    assert!(
        text_top(&output, "Show me a Markdown sample").is_none(),
        "the drawer takes the whole window"
    );
}

#[test]
fn narrow_tab_row_toggles_the_sidebar_without_a_keyboard() {
    let ctx = egui::Context::default();
    let mut app = DesktopApp::open(ctx.clone(), true);
    with_one_conversation(&mut app);
    render_at(&mut app, &ctx, 400.0, Vec::new());
    let output = render_at(&mut app, &ctx, 400.0, Vec::new());
    let toggle = text_pos(&output, "\u{2630}").expect("sidebar toggle in the tab row");
    assert!(
        text_top(&output, "sidebar chat").is_none(),
        "starts collapsed"
    );

    tap_at(&mut app, &ctx, 400.0, toggle + egui::vec2(4.0, 4.0));
    let output = render_at(&mut app, &ctx, 400.0, Vec::new());
    assert!(
        text_top(&output, "sidebar chat").is_some(),
        "the toggle opens the drawer"
    );

    // The drawer covers the tab strip, so the drawer header carries the same
    // toggle to close it again.
    let toggle = text_pos(&output, "\u{2630}").expect("drawer toggle");
    tap_at(&mut app, &ctx, 400.0, toggle + egui::vec2(4.0, 4.0));
    let output = render_at(&mut app, &ctx, 400.0, Vec::new());
    assert!(
        text_top(&output, "sidebar chat").is_none(),
        "the toggle closes it"
    );
    assert!(text_top(&output, "Show me a Markdown sample").is_some());
}

#[test]
fn narrow_windows_stack_panes_even_when_a_side_split_is_asked_for() {
    let ctx = egui::Context::default();
    let mut app = DesktopApp::open(ctx.clone(), true);
    render_at(&mut app, &ctx, 400.0, Vec::new());
    app.split_pane(layout::Axis::Horizontal);
    assert_eq!(
        app.layout.leaves().len(),
        2,
        "the side request still splits"
    );
    let rects = app.layout.rects();
    let (a, b) = (rects[0].1, rects[1].1);
    assert_eq!((a.x, a.w), (b.x, b.w), "narrow panes keep the full width");
    assert!(
        (a.y - b.y).abs() > 0.1 && (a.h + b.h - 1.0).abs() < 1e-3,
        "narrow panes stack top to bottom: {a:?} then {b:?}"
    );
}

#[test]
fn narrow_tab_strip_has_no_actions_menu_and_x_closes_tab() {
    let ctx = egui::Context::default();
    let mut app = DesktopApp::open(ctx.clone(), true);
    render_at(&mut app, &ctx, 400.0, Vec::new());
    render_at(
        &mut app,
        &ctx,
        400.0,
        vec![key_event(egui::Key::T, egui::Modifiers::COMMAND)],
    );
    assert_eq!(app.tabs.len(), 2, "a second tab to close");
    let output = render_at(&mut app, &ctx, 400.0, Vec::new());
    assert!(
        text_pos(&output, "\u{22ee}").is_none(),
        "the phone tab strip has no redundant actions menu"
    );
    let close = text_pos(&output, "×").expect("tab close button");
    tap_at(&mut app, &ctx, 400.0, close + egui::vec2(4.0, 4.0));

    assert_eq!(app.tabs.len(), 1, "the tab close button closes the tab");
    assert_eq!(app.layout.leaves().len(), 1, "and never splits");
}

#[test]
fn narrow_touch_taps_select_and_close_tabs() {
    let (mut app, ctx, _rx) = app();
    render_at(&mut app, &ctx, 400.0, touch_device());
    render_at(
        &mut app,
        &ctx,
        400.0,
        vec![key_event(egui::Key::T, egui::Modifiers::COMMAND)],
    );
    assert_eq!(app.tabs.len(), 2, "a second tab to select");
    let (first, second) = (app.tabs[0].id, app.tabs[1].id);
    assert_eq!(app.active_chat, second, "a new tab starts selected");

    let output = render_at(&mut app, &ctx, 400.0, Vec::new());
    let label = text_pos(&output, "new chat").expect("the first tab's label");
    touch_tap_at(
        &mut app,
        &ctx,
        400.0,
        label + egui::vec2(4.0, 4.0),
        egui::vec2(2.0, 1.0),
    );
    assert_eq!(
        app.active_chat, first,
        "a finger tap selects the tab it lands on"
    );

    let output = render_at(&mut app, &ctx, 400.0, Vec::new());
    let close = text_pos(&output, "×").expect("the first tab's close button");
    touch_tap_at(
        &mut app,
        &ctx,
        400.0,
        close + egui::vec2(4.0, 4.0),
        egui::vec2(2.0, 1.0),
    );
    assert_eq!(app.tabs.len(), 1, "a finger tap on × closes that tab");
    assert_eq!(app.active_chat, second, "and leaves the other one selected");
}

#[test]
fn narrow_touch_taps_tolerate_a_finger_drift() {
    let (mut app, ctx, _rx) = app();
    render_at(&mut app, &ctx, 400.0, touch_device());
    render_at(
        &mut app,
        &ctx,
        400.0,
        vec![key_event(egui::Key::T, egui::Modifiers::COMMAND)],
    );
    assert_eq!(app.tabs.len(), 2, "a tab to leave and a tab to tap");
    // Name the tab once both exist: opening a tab reconnects the chats, and a
    // title written between two openings can be dropped by the next one.
    title_tab_at(&mut app, 0, "/alpha");
    let first = app.tabs[0].id;
    assert_eq!(
        app.active_chat, app.tabs[1].id,
        "the new tab starts selected"
    );

    let output = render_at(&mut app, &ctx, 400.0, Vec::new());
    let label = text_pos(&output, "/alpha").expect("the first tab's label");
    // 7.6 pt of drift: further than a mouse click may wander, closer than the
    // finger-sized window, so this still has to select the tab.
    touch_tap_at(
        &mut app,
        &ctx,
        400.0,
        label + egui::vec2(4.0, 4.0),
        egui::vec2(7.0, 3.0),
    );
    assert_eq!(
        app.active_chat, first,
        "a finger that drifts a little still taps"
    );
}

#[test]
fn narrow_touch_drag_scrolls_the_tab_strip_without_reordering() {
    let (mut app, ctx, _rx) = app();
    render_at(&mut app, &ctx, 400.0, touch_device());
    let titles = [
        "/alpha", "/bravo", "/charlie", "/delta", "/echo", "/foxtrot", "/golf", "/hotel", "/india",
        "/juliet",
    ];
    for _ in 1..titles.len() {
        render_at(
            &mut app,
            &ctx,
            400.0,
            vec![key_event(egui::Key::T, egui::Modifiers::COMMAND)],
        );
    }
    assert_eq!(
        app.tabs.len(),
        titles.len(),
        "more tabs than the phone strip can show"
    );
    for (index, title) in titles.iter().enumerate() {
        title_tab_at(&mut app, index, title);
    }
    let order: Vec<u64> = app.tabs.iter().map(|tab| tab.id).collect();
    let output = render_at(&mut app, &ctx, 400.0, Vec::new());
    let start = text_pos(&output, "/bravo").expect("the second tab's label");

    let button = |pos: egui::Pos2, pressed: bool| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    let from = start + egui::vec2(4.0, 4.0);
    let moved = |dx: f32| from - egui::vec2(dx, 0.0);
    render_at(&mut app, &ctx, 400.0, vec![egui::Event::PointerMoved(from)]);
    render_at(&mut app, &ctx, 400.0, vec![button(from, true)]);
    render_at(
        &mut app,
        &ctx,
        400.0,
        vec![egui::Event::PointerMoved(moved(10.0))],
    );
    let moved_output = render_at(
        &mut app,
        &ctx,
        400.0,
        vec![egui::Event::PointerMoved(moved(30.0))],
    );
    assert!(
        app.tab_dragging.is_none(),
        "a finger drag on a tab scrolls the strip instead of picking the tab up"
    );
    // The scroll offset is eased after the pointer moves; inspect this frame
    // before the strip can settle far enough to cull the target label.
    let scrolled = text_pos(&moved_output, "/bravo").expect("the second tab's label");
    assert!(
        scrolled.x < start.x - 5.0,
        "the finger drag scrolled the strip: {:?} to {:?}",
        start,
        scrolled
    );
    render_at(&mut app, &ctx, 400.0, vec![button(moved(30.0), false)]);
    render_at(&mut app, &ctx, 400.0, Vec::new());

    assert!(app.tab_drop.is_none(), "and drops no tab");
    assert_eq!(
        app.tabs.iter().map(|tab| tab.id).collect::<Vec<_>>(),
        order,
        "the tab order is untouched"
    );
}

#[test]
fn wide_windows_still_split_side_by_side() {
    let ctx = egui::Context::default();
    let mut app = DesktopApp::open(ctx.clone(), true);
    render_at(&mut app, &ctx, 1000.0, Vec::new());
    app.split_pane(layout::Axis::Horizontal);
    let rects = app.layout.rects();
    assert_eq!(rects.len(), 2);
    let (a, b) = (rects[0].1, rects[1].1);
    assert_eq!((a.y, a.h), (b.y, b.h), "wide panes share the height");
    assert!(
        (a.x - b.x).abs() > 0.1 && (a.w + b.w - 1.0).abs() < 1e-3,
        "wide panes sit side by side: {a:?} then {b:?}"
    );
}

#[test]
fn touch_screens_get_a_stop_button_while_a_turn_runs() {
    let (mut app, ctx, mut rx) = app();
    app.session_mut().state.busy = true;
    let touch = egui::Event::Touch {
        device_id: egui::TouchDeviceId(1),
        id: egui::TouchId(1),
        phase: egui::TouchPhase::Start,
        pos: egui::pos2(1.0, 1.0),
        force: None,
    };
    render_at(&mut app, &ctx, 400.0, vec![touch]);
    let output = render_at(&mut app, &ctx, 400.0, Vec::new());
    let stop = output
        .shapes
        .iter()
        .find_map(|clipped| match &clipped.shape {
            egui::Shape::Text(text) if text.galley.job.text.contains("Stop") => Some(text.pos),
            _ => None,
        })
        .expect("Stop button after a touch");
    assert!(
        stop.x > 300.0,
        "pinned to the row's far right, at {}",
        stop.x
    );

    let at = stop + egui::vec2(4.0, 4.0);
    let press = |pressed| egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    render_at(&mut app, &ctx, 400.0, vec![egui::Event::PointerMoved(at)]);
    render_at(&mut app, &ctx, 400.0, vec![press(true), press(false)]);
    assert!(
        sent(&mut rx)
            .iter()
            .any(|command| matches!(command, RuntimeCommand::Cancel)),
        "tapping Stop cancels the turn"
    );
}

#[test]
fn input_presets_frame_the_composer_like_the_tui() {
    let ctx = egui::Context::default();
    let mut app = DesktopApp::open(ctx.clone(), true);
    let mut frame_for = |preset: &str| {
        app.session_mut().state.input_style = state::InputStyle::from_settings(
            &serde_json::json!({ "ui": { "input": { "preset": preset } } }),
        );
        let mut frame = None;
        ctx.run_ui(egui::RawInput::default(), |ui| {
            frame = Some(app.input_frame(ui))
        })
        .textures_delta
        .clear();
        frame.unwrap()
    };
    let lines = frame_for("lines");
    assert_eq!(lines.stroke.width, 0.0);
    assert_eq!(lines.fill, egui::Color32::TRANSPARENT);
    let boxed = frame_for("box");
    assert!(boxed.stroke.width > 0.0, "box draws a border");
    assert!(boxed.inner_margin.left > 0, "box pads its sides");
    let filled = frame_for("filled");
    assert_eq!(filled.stroke.width, 0.0);
    assert_ne!(
        filled.fill,
        egui::Color32::TRANSPARENT,
        "filled paints the input background"
    );
}

fn key_replies(rx: &mut mpsc::UnboundedReceiver<Command>) -> Vec<(u64, bone_protocol::KeyEvent)> {
    sent(rx)
        .into_iter()
        .filter_map(|command| match command {
            RuntimeCommand::KeyReply { id, key } => Some((id, key)),
            _ => None,
        })
        .collect()
}

#[test]
fn tapping_a_menu_line_answers_the_key_request_with_a_click() {
    let (mut session, mut rx) = session();
    session.state.pending_key = Some(7);
    session.click_menu("2".into());
    let replies = key_replies(&mut rx);
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].0, 7);
    assert_eq!(
        (replies[0].1.code.as_str(), replies[0].1.char.as_deref()),
        ("Click", Some("2"))
    );
    assert_eq!(session.state.pending_key, None);
}

#[test]
fn text_typed_into_a_menu_is_sent_one_character_per_key_request() {
    let (mut session, mut rx) = session();
    let ctx = egui::Context::default();
    let frame = |session: &mut Session, events: Vec<egui::Event>| {
        ctx.run_ui(
            egui::RawInput {
                events,
                ..Default::default()
            },
            |ui| session.capture_key(ui),
        )
        .textures_delta
        .clear();
    };
    // An on-screen keyboard delivers `@` as text; there is no egui key for it.
    session.state.pending_key = Some(1);
    frame(&mut session, vec![egui::Event::Text("@x".into())]);
    session.state.pending_key = Some(2);
    frame(&mut session, Vec::new());
    let chars: Vec<_> = key_replies(&mut rx)
        .into_iter()
        .map(|(id, key)| (id, key.code, key.char))
        .collect();
    assert_eq!(
        chars,
        [
            (1, "Char".to_string(), Some("@".to_string())),
            (2, "Char".to_string(), Some("x".to_string()))
        ]
    );
}

#[test]
fn an_open_menu_keeps_the_input_away_and_queues_text_between_key_requests() {
    let (mut app, ctx, mut rx) = app();
    app.session_mut().state.view.components = vec![bone_protocol::Component::Float {
        id: "interact".into(),
        presentation: Default::default(),
        title: "Config".into(),
        lines: vec![bone_protocol::PaneLineSpec::Plain("Edit value".into())],
        rect: bone_protocol::FloatRect {
            anchor: Default::default(),
            width: 0,
            height: 3,
            col: 0,
            row: 0,
        },
        z: 0,
        border: false,
        scroll: 0,
        placement: None,
        owner: None,
    }];
    let frame = |app: &mut DesktopApp, events: Vec<egui::Event>| {
        let mut output = ctx.run_ui(
            egui::RawInput {
                events,
                ..Default::default()
            },
            |ui| app.render(ui),
        );
        output.textures_delta.clear();
        output
    };
    // Between key requests (none pending), the input stays hidden and never
    // takes focus, so the on-screen keyboard is not reset per keystroke.
    let output = frame(&mut app, vec![egui::Event::Text("a".into())]);
    assert!(!ctx.memory(|memory| memory.has_focus(editor_id(app.active_chat))));
    assert!(
        output.platform_output.ime.is_some(),
        "the keyboard stays up"
    );
    assert!(
        app.session().composer.is_empty(),
        "typing does not leak into the input"
    );
    // The next key request gets the character typed in the gap.
    app.session_mut().state.pending_key = Some(4);
    frame(&mut app, Vec::new());
    let replies: Vec<_> = sent(&mut rx)
        .into_iter()
        .filter_map(|command| match command {
            RuntimeCommand::KeyReply { id, key } => Some((id, key.char)),
            _ => None,
        })
        .collect();
    assert_eq!(replies, [(4, Some("a".to_string()))]);
}

#[test]
fn android_back_acts_as_esc() {
    let (mut app, ctx, mut rx) = app();
    let back = || vec![key_event(egui::Key::BrowserBack, egui::Modifiers::NONE)];
    // Leaves a menu: the key request gets Esc.
    app.session_mut().state.pending_key = Some(3);
    frame(&mut app, &ctx, back());
    assert!(matches!(
        sent(&mut rx).as_slice(),
        [RuntimeCommand::KeyReply { id: 3, key }] if key.code == "Esc"
    ));
    // Stops a running turn.
    app.session_mut().state.busy = true;
    frame(&mut app, &ctx, back());
    assert!(matches!(sent(&mut rx).as_slice(), [RuntimeCommand::Cancel]));
}

#[test]
fn desktop_publishes_chat_pane_width_on_connect_and_resize() {
    let (mut app, ctx, mut rx) = app();
    render_at(&mut app, &ctx, 400.0, Vec::new());
    let width = sent(&mut rx)
        .into_iter()
        .find_map(|command| match command {
            RuntimeCommand::SetTerminalWidth { width } => Some(width),
            _ => None,
        })
        .expect("pane width on first frame");
    assert!(width > 20 && width < 80, "phone width: {width}");
    render_at(&mut app, &ctx, 400.0, Vec::new());
    assert!(
        sent(&mut rx)
            .into_iter()
            .all(|command| !matches!(command, RuntimeCommand::SetTerminalWidth { .. }))
    );
    render_at(&mut app, &ctx, 600.0, Vec::new());
    assert!(sent(&mut rx).into_iter().any(|command| matches!(command, RuntimeCommand::SetTerminalWidth { width: next } if next > width)));
    app.session_mut()
        .handle_event(Event::Disconnected("lost link".into()));
    app.session_mut().handle_event(Event::Connected);
    render_at(&mut app, &ctx, 600.0, Vec::new());
    assert!(
        sent(&mut rx)
            .into_iter()
            .any(|command| matches!(command, RuntimeCommand::SetTerminalWidth { .. })),
        "reconnect republishes width"
    );
}

#[test]
fn desktop_republishes_chat_pane_width_after_local_conversation_attachments() {
    let (mut app, ctx, mut rx) = app();
    render_at(&mut app, &ctx, 400.0, Vec::new());
    let width = sent(&mut rx).into_iter().find_map(|command| match command {
        RuntimeCommand::SetTerminalWidth { width } => Some(width),
        _ => None,
    });
    assert!(width.is_some(), "pane width on first frame");

    assert!(app.session_mut().new_conversation());
    assert!(matches!(
        sent(&mut rx).as_slice(),
        [RuntimeCommand::NewConversation]
    ));
    render_at(&mut app, &ctx, 400.0, Vec::new());
    assert!(sent(&mut rx).into_iter().any(|command| matches!(
        command,
        RuntimeCommand::SetTerminalWidth { width: next } if Some(next) == width
    )));

    assert!(app.session_mut().load_conversation(42));
    assert!(matches!(
        sent(&mut rx).as_slice(),
        [RuntimeCommand::LoadConversation { id: 42, .. }]
    ));
    render_at(&mut app, &ctx, 400.0, Vec::new());
    assert!(sent(&mut rx).into_iter().any(|command| matches!(
        command,
        RuntimeCommand::SetTerminalWidth { width: next } if Some(next) == width
    )));

    let action = CommandAction {
        conversation_load: Some(bone_protocol::ConversationLoad {
            messages: Vec::new(),
            conversation_id: Some(43),
        }),
        ..Default::default()
    };
    assert!(app.session_mut().apply_command_action(action).is_none());
    assert!(matches!(
        sent(&mut rx).as_slice(),
        [RuntimeCommand::LoadConversation { id: 43, .. }]
    ));
    render_at(&mut app, &ctx, 400.0, Vec::new());
    assert!(sent(&mut rx).into_iter().any(|command| matches!(
        command,
        RuntimeCommand::SetTerminalWidth { width: next } if Some(next) == width
    )));
}

#[cfg(unix)]
fn recovery_test_child() -> std::process::Child {
    std::process::Command::new("sh")
        .args(["-c", "exec sleep 60"])
        .spawn()
        .expect("spawn recovery test child")
}

#[cfg(unix)]
fn recovery_process_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(unix)]
fn mark_wedged(app: &mut DesktopApp, generation: u64) {
    let session = app.session_mut();
    session.connected = false;
    session.connecting = false;
    session.connect_failed_refused = false;
    session.connect_failed_other = true;
    session.forced_gen = generation;
    session.state.ready = false;
    session.connection_status =
        "Connect failed: 127.0.0.1:7878 did not reach the daemon within 10 seconds".into();
}

#[cfg(unix)]
#[test]
fn wedged_owned_daemon_is_killed_and_replaced() {
    let (mut app, ctx, _rx) = app();
    app.daemon_phase = daemon::Phase::Starting { attempts: 0 };
    app.daemon_bin = Some("true".into());
    let child = recovery_test_child();
    let old_pid = child.id();
    app.daemon_child = Some(child);
    let generation = app.daemon_gen;
    mark_wedged(&mut app, generation);

    app.pump_daemon(&ctx);

    assert_eq!(app.wedged_respawns, 1);
    assert_eq!(app.daemon_gen, generation + 1);
    let new_pid = app
        .daemon_child
        .as_ref()
        .expect("respawned daemon child")
        .id();
    assert_ne!(new_pid, old_pid);
    assert!(
        !recovery_process_alive(old_pid),
        "old daemon was not killed"
    );
    assert!(matches!(app.daemon_phase, daemon::Phase::Starting { .. }));
    if let Some(mut child) = app.daemon_child.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

#[cfg(unix)]
#[test]
fn wedged_owned_daemon_is_killed_when_respawn_budget_is_exhausted() {
    let (mut app, ctx, _rx) = app();
    app.daemon_phase = daemon::Phase::Starting { attempts: 0 };
    app.wedged_respawns = daemon::MAX_WEDGED_RESPAWNS;
    let child = recovery_test_child();
    let pid = child.id();
    app.daemon_child = Some(child);
    let generation = app.daemon_gen;
    mark_wedged(&mut app, generation);

    app.pump_daemon(&ctx);

    assert!(app.daemon_child.is_none());
    let failure = app.connection_failure().expect("exhaustion failure");
    assert!(failure.contains("tried 4 times (3 respawns)"), "{failure}");
    assert!(
        !recovery_process_alive(pid),
        "exhausted daemon was not killed"
    );
}

#[cfg(unix)]
#[test]
fn wedged_foreign_daemon_is_reported_without_being_killed() {
    let (mut app, ctx, _rx) = app();
    app.daemon_phase = daemon::Phase::Probe;
    let mut foreign = recovery_test_child();
    let pid = foreign.id();
    let generation = app.daemon_gen;
    mark_wedged(&mut app, generation);

    app.pump_daemon(&ctx);

    assert!(app.daemon_child.is_none());
    assert!(matches!(app.daemon_phase, daemon::Phase::Stopped(_)));
    assert!(recovery_process_alive(pid), "foreign daemon was killed");
    let _ = foreign.kill();
    let _ = foreign.wait();
}

#[cfg(unix)]
#[test]
fn stale_wedged_generation_does_not_trigger_another_respawn() {
    let (mut app, ctx, _rx) = app();
    app.daemon_phase = daemon::Phase::Starting { attempts: 0 };
    app.daemon_gen = 1;
    let child = recovery_test_child();
    let pid = child.id();
    app.daemon_child = Some(child);
    mark_wedged(&mut app, 0);

    app.pump_daemon(&ctx);

    assert_eq!(app.daemon_gen, 1);
    assert_eq!(app.wedged_respawns, 0);
    assert_eq!(
        app.daemon_child
            .as_ref()
            .expect("original daemon child")
            .id(),
        pid
    );
    assert!(
        recovery_process_alive(pid),
        "stale report killed the daemon"
    );
    assert_eq!(app.daemon_phase, daemon::Phase::Starting { attempts: 0 });
    if let Some(mut child) = app.daemon_child.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

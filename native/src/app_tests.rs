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
    assert!(!ctx.memory(|memory| memory.has_focus(editor_id())));
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

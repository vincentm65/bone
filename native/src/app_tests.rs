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

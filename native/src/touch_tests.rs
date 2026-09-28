use super::*;

use bone_protocol::{CatalogSnapshot, ProviderChoice, SetupSnapshot};
use bone_render::screens::setup::SetupScreen;
use bone_render::screens::{Key, KeyCode};
use bone_render::theme::Theme;
use eframe::egui;

fn setup_page() -> pages::Page {
    let mut screen = SetupScreen::new(
        false,
        SetupSnapshot {
            config_revision: 1,
            providers: vec![ProviderChoice {
                id: "demo".into(),
                label: "Demo".into(),
                api_key_configured: false,
                api_key_required: true,
            }],
            active_provider: "demo".into(),
            init_exists: false,
            needs_onboarding: false,
            catalog: CatalogSnapshot {
                revision: "catalog-1".into(),
                items: Vec::new(),
            },
        },
        &Theme::default(),
    );
    assert!(matches!(
        screen.handle_key(Key::plain(KeyCode::Right)),
        bone_render::screens::setup::SetupAction::None
    ));
    pages::Page {
        screen: pages::Screen::Setup(Some(screen)),
        title: "setup".into(),
        chat: 1,
        pending: None,
        error: None,
    }
}

fn setup_app(ctx: &egui::Context) -> DesktopApp {
    let mut app = DesktopApp::open(ctx.clone(), false);
    app.tabs[0].kind = TabKind::Page(Box::new(setup_page()));
    app
}

fn geometry() -> grid::ScreenGeometry {
    grid::ScreenGeometry {
        rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(320.0, 400.0)),
        cell: 8.0,
        row_height: 20.0,
        cols: 40,
        row_count: 20,
    }
}

fn raw(events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(320.0, 400.0),
        )),
        events,
        ..Default::default()
    }
}

fn touch_event() -> egui::Event {
    egui::Event::Touch {
        device_id: egui::TouchDeviceId(1),
        id: egui::TouchId(1),
        phase: egui::TouchPhase::Start,
        pos: egui::pos2(4.0, 4.0),
        force: None,
    }
}

fn key_event(key: egui::Key) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    }
}

fn focus_editor(app: &mut DesktopApp, ctx: &egui::Context, geometry: &grid::ScreenGeometry) {
    let mut output = ctx.run_ui(raw(vec![touch_event()]), |ui| {
        app.focus_setup_api_key(ui, 1);
        app.setup_api_key_editor(ui, 1, geometry);
    });
    output.textures_delta.clear();
}

fn editor_frame(
    app: &mut DesktopApp,
    ctx: &egui::Context,
    geometry: &grid::ScreenGeometry,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    ctx.run_ui(raw(events), |ui| {
        app.handle_keys(ui);
        app.setup_api_key_editor(ui, 1, geometry);
    })
}

fn api_key(app: &mut DesktopApp) -> String {
    app.selected_page()
        .expect("setup page")
        .setup_api_key_mut()
        .expect("provider API-key field")
        .clone()
}

#[test]
fn setup_api_key_editor_requires_narrow_touch_layout() {
    let ctx = egui::Context::default();
    let mut app = setup_app(&ctx);
    let geometry = geometry();

    focus_editor(&mut app, &ctx, &geometry);
    assert!(app.page_input_focus.is_none());
    assert_eq!(ctx.memory(|memory| memory.focused()), None);

    app.touch = true;
    focus_editor(&mut app, &ctx, &geometry);
    assert_eq!(app.page_input_focus, Some(1));
    assert_eq!(
        ctx.memory(|memory| memory.focused()),
        Some(setup_api_key_editor_id(1))
    );
}

#[test]
fn setup_api_key_editor_syncs_text_paste_editing_and_ime_events() {
    let ctx = egui::Context::default();
    let mut app = setup_app(&ctx);
    app.touch = true;
    *app.selected_page()
        .expect("setup page")
        .setup_api_key_mut()
        .expect("provider API-key field") = "ab".into();
    let geometry = geometry();
    focus_editor(&mut app, &ctx, &geometry);

    let mut output = editor_frame(
        &mut app,
        &ctx,
        &geometry,
        vec![egui::Event::Text("C".into())],
    );
    output.textures_delta.clear();
    assert_eq!(api_key(&mut app), "abC");

    let mut output = editor_frame(
        &mut app,
        &ctx,
        &geometry,
        vec![egui::Event::Paste("DE".into())],
    );
    output.textures_delta.clear();
    assert_eq!(api_key(&mut app), "abCDE");

    let mut output = editor_frame(
        &mut app,
        &ctx,
        &geometry,
        vec![key_event(egui::Key::ArrowLeft)],
    );
    output.textures_delta.clear();
    let mut output = editor_frame(
        &mut app,
        &ctx,
        &geometry,
        vec![egui::Event::Text("X".into())],
    );
    output.textures_delta.clear();
    assert_eq!(api_key(&mut app), "abCDXE");

    let mut output = editor_frame(
        &mut app,
        &ctx,
        &geometry,
        vec![key_event(egui::Key::Backspace)],
    );
    output.textures_delta.clear();
    let mut output = editor_frame(
        &mut app,
        &ctx,
        &geometry,
        vec![key_event(egui::Key::Delete)],
    );
    output.textures_delta.clear();
    assert_eq!(api_key(&mut app), "abCD");

    let mut output = editor_frame(
        &mut app,
        &ctx,
        &geometry,
        vec![egui::Event::Ime(egui::ImeEvent::Preedit {
            text: "候".into(),
            active_range_chars: Some(0..1),
        })],
    );
    output.textures_delta.clear();
    assert_eq!(api_key(&mut app), "abCD候");

    let output = editor_frame(
        &mut app,
        &ctx,
        &geometry,
        vec![egui::Event::Ime(egui::ImeEvent::Commit("漢".into()))],
    );
    assert_eq!(api_key(&mut app), "abCD漢");
    assert!(
        output.platform_output.ime.is_some(),
        "the focused editor keeps emitting an IME target"
    );
}

#[test]
fn focused_touch_api_key_editor_surrenders_on_first_android_back() {
    let ctx = egui::Context::default();
    let mut app = setup_app(&ctx);
    app.touch = true;
    let geometry = geometry();
    focus_editor(&mut app, &ctx, &geometry);
    assert!(app.page_api_key_input_active());

    let mut output = ctx.run_ui(raw(vec![key_event(egui::Key::BrowserBack)]), |ui| {
        app.handle_keys(ui)
    });
    output.textures_delta.clear();

    assert!(app.page_input_focus.is_none());
    assert_eq!(ctx.memory(|memory| memory.focused()), None);
    assert!(matches!(app.tabs[app.selected].kind, TabKind::Page(_)));
}

fn stats_page() -> pages::Page {
    use bone_protocol::{UsageStatsSnapshot, UsageSummary};
    let (mut screen, _) = bone_render::screens::stats::StatsScreen::new(&Theme::default());
    screen.loaded(Ok(UsageStatsSnapshot {
        started_at: None,
        ended_at: None,
        total: UsageSummary::default(),
        by_model_today: Vec::new(),
        by_model_7d: Vec::new(),
        by_model_4w: Vec::new(),
        by_model_all: Vec::new(),
        daily: Vec::new(),
        weekly: Vec::new(),
        monthly: Vec::new(),
        all_time: Vec::new(),
        yearly: Vec::new(),
        hourly_today: Vec::new(),
        hourly_7d: Vec::new(),
        hourly_4w: Vec::new(),
        hourly_all: Vec::new(),
        daily_activity: Vec::new(),
    }));
    pages::Page {
        screen: pages::Screen::Stats(Box::new(screen)),
        title: "stats".into(),
        chat: 1,
        pending: None,
        error: None,
    }
}

/// Drive one real egui tap (press frame, release frame) through `page_touch`.
fn tap(
    app: &mut DesktopApp,
    ctx: &egui::Context,
    geometry: &grid::ScreenGeometry,
    row: u16,
    col: u16,
) {
    let pos = egui::pos2(
        geometry.rect.left() + (col as f32 + 0.5) * geometry.cell,
        geometry.rect.top() + (row as f32 + 0.5) * geometry.row_height,
    );
    let tab_id = app.tabs[app.selected].id;
    let button = |pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    for events in [
        Vec::new(),
        vec![egui::Event::PointerMoved(pos), button(true)],
        vec![button(false)],
        Vec::new(),
    ] {
        let mut output = ctx.run_ui(raw(events), |ui| app.page_touch(ui, tab_id, geometry));
        output.textures_delta.clear();
    }
}

fn page_row(app: &mut DesktopApp, geometry: &grid::ScreenGeometry, row: u16) -> String {
    let page = app.selected_page().expect("page");
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(
        geometry.cols,
        geometry.row_count,
    ))
    .expect("terminal");
    let theme = Theme::default();
    terminal
        .draw(|frame| page.draw(frame, &theme))
        .expect("draw");
    let buffer = terminal.backend().buffer();
    (0..geometry.cols)
        .map(|x| buffer[(x, row)].symbol().to_string())
        .collect()
}

#[test]
fn tapping_stats_tabs_and_view_hint_switches_views() {
    let ctx = egui::Context::default();
    let mut app = DesktopApp::open(ctx.clone(), false);
    app.tabs[0].kind = TabKind::Page(Box::new(stats_page()));
    app.touch = true;
    let geometry = geometry();
    let dump: Vec<String> = (0..geometry.row_count)
        .map(|row| format!("{row:2}|{}|", page_row(&mut app, &geometry, row)))
        .collect();
    assert!(
        page_row(&mut app, &geometry, 2).contains("[2 7 days]"),
        "initial layout:\n{}",
        dump.join("\n")
    );

    tap(&mut app, &ctx, &geometry, 2, 3);
    assert!(
        page_row(&mut app, &geometry, 2).contains("[1 Today]"),
        "tab tap: {}",
        page_row(&mut app, &geometry, 2)
    );

    let view = " q/Esc quit  ".chars().count() as u16 + 2;
    tap(&mut app, &ctx, &geometry, geometry.row_count - 1, view);
    assert!(
        page_row(&mut app, &geometry, 2).contains("[2 7 days]"),
        "view hint tap: {}",
        page_row(&mut app, &geometry, 2)
    );
}

fn catalog_page() -> pages::Page {
    use bone_protocol::CatalogItem;
    use bone_render::screens::catalog::CatalogScreen;
    let item = |name: &str| CatalogItem {
        name: name.into(),
        kind: "tool".into(),
        description: "demo".into(),
        ..CatalogItem::default()
    };
    let screen = CatalogScreen::new(
        CatalogSnapshot {
            revision: "catalog-1".into(),
            items: vec![item("alpha"), item("beta"), item("gamma")],
        },
        &Theme::default(),
    );
    pages::Page {
        screen: pages::Screen::Catalog(Some(screen)),
        title: "catalog".into(),
        chat: 1,
        pending: None,
        error: None,
    }
}

#[test]
fn tapping_catalog_rows_and_footer_moves_and_toggles() {
    let ctx = egui::Context::default();
    let mut app = DesktopApp::open(ctx.clone(), false);
    app.tabs[0].kind = TabKind::Page(Box::new(catalog_page()));
    app.touch = true;
    let geometry = geometry();
    let dump = |app: &mut DesktopApp| -> String {
        (0..geometry.row_count)
            .map(|row| format!("{row:2}|{}|", page_row(app, &geometry, row)))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let initial = dump(&mut app);
    assert!(
        page_row(&mut app, &geometry, 8).contains("▸ [ ] alpha")
            && page_row(&mut app, &geometry, 9).contains("beta"),
        "initial layout:\n{initial}"
    );

    tap(&mut app, &ctx, &geometry, 9, 6);
    let after_row = dump(&mut app);
    assert!(
        page_row(&mut app, &geometry, 9).contains("▸ [x] beta"),
        "row tap:\n{after_row}"
    );

    tap(&mut app, &ctx, &geometry, geometry.row_count - 1, 2);
    let after_footer = dump(&mut app);
    assert!(
        page_row(&mut app, &geometry, 10).contains("▸ [ ] gamma"),
        "footer tap:\n{after_footer}"
    );
}

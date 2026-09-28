use super::*;
use ratatui::style::Color;

#[test]
fn setup_options_and_summary_use_semantic_palette_roles() {
    let mut theme = Theme::default();
    theme.palette.good = Color::Rgb(1, 2, 3);
    theme.palette.subtle = Color::Rgb(4, 5, 6);
    theme.palette.accent = Color::Rgb(7, 8, 9);
    theme.palette.fg = Color::Rgb(10, 11, 12);
    theme.palette.muted = Color::Rgb(13, 14, 15);

    let lines = vec![
        radio_option("Selected".into(), true, &theme),
        radio_option("Inactive".into(), false, &theme),
        summary("Provider", "configured".into(), &theme),
    ];
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(50, 3)).unwrap();
    terminal
        .draw(|frame| frame.render_widget(Paragraph::new(lines), frame.area()))
        .unwrap();
    let buffer = terminal.backend().buffer();

    assert_eq!(buffer.cell((1, 0)).unwrap().fg, theme.palette.good);
    assert_eq!(buffer.cell((3, 0)).unwrap().fg, theme.palette.accent);
    assert_eq!(buffer.cell((1, 1)).unwrap().fg, theme.palette.subtle);
    assert_eq!(buffer.cell((3, 1)).unwrap().fg, theme.palette.fg);
    assert_eq!(buffer.cell((2, 2)).unwrap().fg, theme.palette.muted);
    assert_eq!(buffer.cell((13, 2)).unwrap().fg, theme.palette.fg);
}

#[test]
fn seeded_provider_is_submitted_without_an_api_key() {
    let snapshot = SetupSnapshot {
        config_revision: 4,
        providers: vec![bone_protocol::ProviderChoice {
            id: "local".into(),
            label: "Local".into(),
            api_key_configured: false,
            api_key_required: false,
        }],
        active_provider: "local".into(),
        init_exists: false,
        needs_onboarding: true,
        catalog: bone_protocol::CatalogSnapshot {
            revision: "catalog-1".into(),
            items: Vec::new(),
        },
    };
    let state = State::new(true, snapshot, &Theme::default());
    let plan = plan(&state);

    assert_eq!(plan.provider_id.as_deref(), Some("local"));
    assert_eq!(plan.api_key, None);
    assert_eq!(plan.expected_config_revision, 4);
}

fn setup_snapshot(
    providers: Vec<bone_protocol::ProviderChoice>,
    items: Vec<bone_protocol::CatalogItem>,
) -> SetupSnapshot {
    SetupSnapshot {
        config_revision: 1,
        providers,
        active_provider: "p0".into(),
        init_exists: true,
        needs_onboarding: false,
        catalog: bone_protocol::CatalogSnapshot {
            revision: "catalog-1".into(),
            items,
        },
    }
}

fn provider(id: usize) -> bone_protocol::ProviderChoice {
    bone_protocol::ProviderChoice {
        id: format!("p{id}"),
        label: format!("Provider {id}"),
        api_key_configured: false,
        api_key_required: true,
    }
}

fn catalog_item(name: &str) -> bone_protocol::CatalogItem {
    bone_protocol::CatalogItem {
        name: name.into(),
        description: "item".into(),
        ..bone_protocol::CatalogItem::default()
    }
}

#[test]
fn touch_provider_rows_honor_the_visible_window_and_key_field() {
    let mut screen = SetupScreen::new(
        false,
        setup_snapshot((0..12).map(provider).collect(), Vec::new()),
        &Theme::default(),
    );
    screen.state.step = Step::Provider;
    screen.state.provider_cursor = 7;

    let TouchAction::Keys(keys) = screen.touch_key(8, 4, 80, 20).expect("provider row") else {
        panic!("provider row should use cursor keys");
    };
    assert_eq!(keys.len(), 4);
    assert!(keys.iter().all(|key| key.code == KeyCode::Up));

    assert_eq!(
        screen.touch_key(16, 4, 80, 20),
        Some(TouchAction::ApiKey),
        "the masked field remains a touch-only focus target"
    );
    let keys = footer_keys(Step::Provider, false);
    let (_, start, _) = picker::footer_hit(0, &keys).expect("first footer token");
    let (_, type_start, _) = picker::footer_hit(start + 15, &keys).expect("type footer token");
    assert_eq!(
        screen.touch_key(18, type_start + 1, 80, 20),
        Some(TouchAction::ApiKey)
    );
}

#[test]
fn touch_setup_checklist_and_init_rows_feed_existing_keys() {
    let mut screen = SetupScreen::new(
        false,
        setup_snapshot(
            vec![provider(0)],
            vec![catalog_item("one"), catalog_item("two")],
        ),
        &Theme::default(),
    );
    screen.state.step = Step::Catalog;
    screen.state.cat_cursor = 1;
    let TouchAction::Keys(keys) = screen.touch_key(8, 4, 80, 20).expect("catalog row") else {
        panic!("catalog row should use cursor and toggle keys");
    };
    assert_eq!(keys.last().map(|key| key.code), Some(KeyCode::Char(' ')));
    assert_eq!(keys.first().map(|key| key.code), Some(KeyCode::Down));

    screen.state.step = Step::Init;
    screen.state.init_cursor = 0;
    let TouchAction::Keys(keys) = screen.touch_key(9, 4, 80, 20).expect("init row") else {
        panic!("init row should use cursor keys");
    };
    assert_eq!(keys, vec![Key::plain(KeyCode::Down)]);

    let keys = footer_keys(Step::Init, false);
    let (_, _, first_end) = picker::footer_hit(0, &keys).unwrap();
    let (_, next_start, _) = picker::footer_hit(first_end, &keys).unwrap();
    assert_eq!(
        screen.touch_key(18, next_start + 1, 80, 20),
        Some(TouchAction::Keys(vec![Key::plain(KeyCode::Right)]))
    );
}

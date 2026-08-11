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

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
fn seeded_provider_can_be_activated_without_api_key() {
    let _guard = crate::ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let previous = std::env::var_os("BONE_DIR");
    let root = std::env::temp_dir().join(format!(
        "bone-setup-provider-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    unsafe { std::env::set_var("BONE_DIR", &root) };

    let store = config::store::ConfigStore::new(crate::ext::ExtensionManager::unloaded())
        .expect("seed fresh configuration");
    assert!(
        store.providers_config().providers["local"]
            .api_key
            .is_empty()
    );
    assert!(activate_provider(&store, "local"));
    assert_eq!(store.providers_config().last_provider, "local");

    std::fs::remove_dir_all(root).ok();
    unsafe {
        match previous {
            Some(value) => std::env::set_var("BONE_DIR", value),
            None => std::env::remove_var("BONE_DIR"),
        }
    }
}

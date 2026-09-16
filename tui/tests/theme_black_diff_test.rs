//! End-to-end check that the catalog `black` theme's diff roles survive a real
//! boot: `lua/themes/black.lua` → resolved settings → rendered chat spans.
//!
//! The sibling `bone-catalog` checkout is optional for this workspace, so the
//! test skips when the theme file is absent (same convention as
//! `common::seed_catalog_into`).

mod common;

use std::path::PathBuf;

use bone::chat::Message;
use bone::ui::render::messages::msg_to_lines;
use bone::ui::theme::Theme;
use ratatui::style::Color;
use ratatui::text::Line;

fn catalog_dir() -> PathBuf {
    std::env::var_os("BONE_CATALOG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../bone-catalog"))
}

fn preview_band(lines: &[Line<'static>], bg: Color) -> Line<'static> {
    lines
        .iter()
        .find(|line| line.spans.first().and_then(|span| span.style.bg) == Some(bg))
        .unwrap_or_else(|| panic!("no diff line with band {bg:?}"))
        .clone()
}

#[test]
fn black_catalog_theme_applies_diff_text_and_band_to_rendered_spans() {
    let theme_path = catalog_dir().join("themes/black.lua");
    let Ok(source) = std::fs::read_to_string(&theme_path) else {
        eprintln!(
            "skipping: {} is not present in this checkout",
            theme_path.display()
        );
        return;
    };

    let config_dir = common::temp_dir("theme-black-diff");
    let _bone_dir = common::isolate_bone_dir(&config_dir);
    std::fs::create_dir_all(config_dir.join("lua/themes")).unwrap();
    std::fs::write(
        config_dir.join("config.yaml"),
        "version: 2\ntheme:\n  name: black\n",
    )
    .unwrap();
    std::fs::write(config_dir.join("lua/themes/black.lua"), &source).unwrap();

    let config =
        bone::config::store::ConfigStore::new(bone::ext::ExtensionManager::unloaded()).unwrap();
    let settings = config.runtime_settings_handle();
    bone::ext::boot_with_tools_shared(
        &config_dir,
        &config_dir,
        &config,
        false,
        bone::ext::BootOptions::default(),
        "test-model",
        "TestProvider",
        settings.clone(),
    );

    let theme_settings = settings
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .resolved()
        .theme
        .clone();
    assert_eq!(theme_settings.name.as_deref(), Some("black"));
    assert_eq!(theme_settings.diff_removed.as_deref(), Some("#ff7b72"));
    assert_eq!(theme_settings.diff_removed_bg.as_deref(), Some("#222222"));
    assert_eq!(theme_settings.diff_added.as_deref(), Some("#7ee787"));
    assert_eq!(theme_settings.diff_added_bg.as_deref(), Some("#1a1a1a"));

    let theme = Theme::from_snapshot(&theme_settings);
    assert_eq!(theme.diff_removed, Some(Color::Rgb(0xff, 0x7b, 0x72)));
    assert_eq!(theme.diff_removed_bg, Color::Rgb(0x22, 0x22, 0x22));
    assert_eq!(theme.diff_added, Some(Color::Rgb(0x7e, 0xe7, 0x87)));
    assert_eq!(theme.diff_added_bg, Color::Rgb(0x1a, 0x1a, 0x1a));

    let lines = msg_to_lines(
        &[Message::system(
            "\nfile.rs | -1 | +1\n   12 - old value\n   12 + new value",
        )],
        &theme,
        None,
        40,
        false,
    );
    let removed = preview_band(&lines, theme.diff_removed_bg);
    let added = preview_band(&lines, theme.diff_added_bg);
    assert_eq!(
        removed.spans[0].style.fg,
        Some(Color::Rgb(0xff, 0x7b, 0x72))
    );
    assert_eq!(added.spans[0].style.fg, Some(Color::Rgb(0x7e, 0xe7, 0x87)));

    std::fs::remove_dir_all(&config_dir).ok();
}

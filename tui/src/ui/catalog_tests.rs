use super::*;

fn entry(kind: &str) -> CatalogEntry {
    CatalogEntry {
        name: "demo.lua".to_string(),
        kind: kind.to_string(),
        description: "Demo extension".to_string(),
        ..CatalogEntry::default()
    }
}

fn make_theme() -> Theme {
    Theme::default()
}

fn result_state(banner: &str) -> State {
    State {
        entries: Vec::new(),
        items: vec![Item::new("demo.lua".into(), "Demo".into(), false)],
        cursor: 0,
        outcome: Outcome {
            changed: false,
            message: String::new(),
        },
        result: Some(banner.to_string()),
    }
}

#[test]
fn result_overlays_use_good_and_error_roles() {
    let mut theme = make_theme();
    theme.palette.good = ratatui::style::Color::Rgb(1, 2, 3);
    theme.palette.error = ratatui::style::Color::Rgb(4, 5, 6);
    let mut items = vec![
        Item::new("installed".into(), String::new(), false),
        Item::new("removed".into(), String::new(), false),
        Item::new("failed".into(), String::new(), false),
        Item::new("unchanged".into(), String::new(), false),
    ];
    items[3].tag = Some("keep".into());
    let results = vec![
        ("installed".into(), ItemResult::Installed),
        ("removed".into(), ItemResult::Removed),
        ("failed".into(), ItemResult::Failed("network".into())),
        ("unchanged".into(), ItemResult::Unchanged),
    ];

    overlay_results(&mut items, &results, &theme);

    assert_eq!(items[0].tag_color, Some(theme.palette.good));
    assert_eq!(items[1].tag_color, Some(theme.palette.good));
    assert_eq!(items[2].tag_color, Some(theme.palette.error));
    assert_eq!(items[2].desc, "Failed: network");
    assert_eq!(items[3].tag.as_deref(), Some("keep"));
    assert_eq!(items[3].tag_color, None);
}

#[test]
fn result_banners_render_with_success_and_error_roles() {
    let mut theme = make_theme();
    theme.palette.good = ratatui::style::Color::Rgb(7, 8, 9);
    theme.palette.error = ratatui::style::Color::Rgb(10, 11, 12);

    for (banner, expected) in [
        ("✓ installed 1", theme.palette.good),
        ("✗ 1 failed", theme.palette.error),
    ] {
        let state = result_state(banner);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 14)).unwrap();
        terminal
            .draw(|frame| draw_body(frame, frame.area(), &state, &theme))
            .unwrap();
        assert_eq!(
            terminal.backend().buffer().cell((2, 1)).unwrap().fg,
            expected
        );
    }
}

#[test]
fn available_item_is_unchecked_without_status() {
    let theme = make_theme();
    let item = build_item(&entry("tool"), false, false, &theme);

    assert!(!item.checked);
    assert!(!item.user_touched);
    assert_eq!(item.tag, None);
    assert_eq!(item.category, "tool");
}

#[test]
fn installed_item_is_checked_and_labeled() {
    let theme = make_theme();
    let item = build_item(&entry("command"), true, false, &theme);

    assert!(item.checked);
    assert!(!item.user_touched);
    assert_eq!(item.tag.as_deref(), Some("installed"));
    assert_eq!(item.tag_color, Some(theme.palette.good));
    assert_eq!(item.category, "command");
}

#[test]
fn theme_item_has_distinct_category() {
    let theme = make_theme();
    let item = build_item(&entry("theme"), false, false, &theme);

    assert_eq!(item.category, "theme");
}

#[test]
fn pending_update_takes_precedence_and_is_applied_by_default() {
    let theme = make_theme();
    let item = build_item(&entry("tool"), true, true, &theme);

    assert!(item.checked);
    assert!(item.user_touched);
    assert_eq!(item.tag.as_deref(), Some("update"));
    assert_eq!(item.tag_color, Some(theme.palette.accent));
}

#[test]
fn metadata_is_added_to_the_detail_pane() {
    let mut entry = entry("tool");
    entry.version = Some("1.2.3".to_string());
    entry.updated_at = Some("2026-03-10".to_string());
    entry.author = Some("Bone Team".to_string());
    entry.repository = Some("https://example.com/repo".to_string());
    entry.documentation = Some("https://example.com/docs".to_string());
    entry.min_bone_version = Some(">=2.4".to_string());
    entry.dependencies = vec!["helper.lua".to_string()];
    entry.permissions = vec!["network".to_string(), "filesystem".to_string()];
    entry.long_description = Some("A longer explanation.".to_string());

    let theme = make_theme();
    let item = build_item(&entry, false, false, &theme);

    assert_eq!(item.details.len(), 8);
    assert_eq!(
        item.details[0],
        ("Version".to_string(), "1.2.3".to_string())
    );
    assert_eq!(item.details[7].0, "Permissions");
    assert_eq!(item.long_desc.as_deref(), Some("A longer explanation."));
}

#[test]
fn rows_are_grouped_as_updates_installed_and_available() {
    let theme = make_theme();
    let entries = vec![
        CatalogEntry {
            name: "available.lua".to_string(),
            ..entry("tool")
        },
        CatalogEntry {
            name: "update.lua".to_string(),
            ..entry("tool")
        },
        CatalogEntry {
            name: "installed.lua".to_string(),
            ..entry("command")
        },
    ];
    let items = vec![
        build_item(&entries[0], false, false, &theme),
        build_item(&entries[1], true, true, &theme),
        build_item(&entries[2], true, false, &theme),
    ];

    let (entries, items) = group_rows(entries, items);

    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["update.lua", "installed.lua", "available.lua"]
    );
    assert_eq!(items[0].section.as_deref(), Some("Updates (1)"));
    assert_eq!(items[1].section.as_deref(), Some("Installed (1)"));
    assert_eq!(items[2].section.as_deref(), Some("Available (1)"));

    let width = 100;
    let height = 20;
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| {
            picker::draw_list(
                frame,
                frame.area(),
                "Catalog",
                "Grouped extensions",
                &items,
                0,
                &theme,
            );
        })
        .unwrap();
    let screen = (0..height)
        .map(|row| {
            (0..width)
                .map(|column| {
                    terminal
                        .backend()
                        .buffer()
                        .cell((column, row))
                        .unwrap()
                        .symbol()
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(screen.contains("Updates (1)"), "{screen}");
    assert!(screen.contains("Installed (1)"), "{screen}");
    assert!(screen.contains("Available (1)"), "{screen}");
}

#[test]
fn theme_render_assertion() {
    let theme = Theme::default();
    let item = build_item(&entry("tool"), true, false, &theme);
    assert_eq!(item.tag.as_deref(), Some("installed"));
    assert_eq!(item.tag_color, Some(theme.palette.good));
}

use super::*;

#[test]
fn set_highlight_sets_resets_and_rejects() {
    let mut theme = Theme::default();
    let original = theme.input_border;

    // A valid color is applied and reports a change.
    assert!(theme.set_highlight("input_border", Some("#ff0000")));
    assert_eq!(theme.input_border, Color::Rgb(255, 0, 0));

    // None resets to the built-in default.
    assert!(theme.set_highlight("input_border", None));
    assert_eq!(theme.input_border, original);

    // Every shell_* highlight group is settable and resettable.
    let read = |t: &Theme, field: &str| -> Option<Color> {
        match field {
            "shell_program" => Some(t.shell_program),
            "shell_separator" => Some(t.shell_separator),
            "shell_redirect" => Some(t.shell_redirect),
            "shell_flag" => Some(t.shell_flag),
            "shell_string" => Some(t.shell_string),
            "shell_variable" => Some(t.shell_variable),
            "shell_comment" => Some(t.shell_comment),
            "shell_path" => Some(t.shell_path),
            _ => None,
        }
    };
    let defaults = Theme::default();
    for field in [
        "shell_program",
        "shell_separator",
        "shell_redirect",
        "shell_flag",
        "shell_string",
        "shell_variable",
        "shell_comment",
        "shell_path",
    ] {
        assert!(
            theme.set_highlight(field, Some("#00ff00")),
            "{field} should set"
        );
        assert_eq!(
            read(&theme, field),
            Some(Color::Rgb(0, 255, 0)),
            "{field} mismatch"
        );
        assert!(theme.set_highlight(field, None), "{field} should reset");
        assert_eq!(read(&theme, field), read(&defaults, field), "{field} reset");
    }

    // Unknown group and unparseable color report no change.
    assert!(!theme.set_highlight("nope", Some("#ffffff")));
    assert!(!theme.set_highlight("input_border", Some("not-a-color")));
}

#[test]
fn invalid_snapshot_runtime_override_does_not_mutate_state() {
    let mut theme = Theme::default();
    let configured = crate::config::settings::ThemeSettings {
        input_border: Some("#654321".into()),
        ..Default::default()
    };
    theme.apply_snapshot(&configured);
    assert!(theme.set_highlight("input_border", Some("#123456")));
    let before = theme.input_border;
    let before_configured = theme.configured_theme().input_border;
    let before_overrides = theme.runtime_overrides().clone();
    let before_code = theme.code().clone();
    let invalid =
        std::collections::HashMap::from([(String::from("nope"), String::from("#ffffff"))]);

    theme.apply_resolved_snapshot(&Default::default(), &invalid);

    assert_eq!(theme.input_border, before);
    assert_eq!(theme.configured_theme().input_border, before_configured);
    assert_eq!(theme.runtime_overrides(), &before_overrides);
    assert_eq!(theme.code().settings, before_code.settings);
    assert_eq!(theme.code().scopes, before_code.scopes);
}

#[test]
fn input_highlights_set_reset_and_follow_palette_defaults() {
    let mut theme = Theme::default();
    assert_eq!(theme.input_bg, theme.palette.selection);
    assert_eq!(theme.input_prefix, theme.palette.fg);
    assert_eq!(theme.input_cursor, theme.palette.fg);

    for name in ["input_bg", "input_prefix", "input_cursor"] {
        assert!(theme.set_highlight(name, Some("#123456")));
    }
    assert_eq!(theme.input_bg, Color::Rgb(0x12, 0x34, 0x56));
    assert_eq!(theme.input_prefix, Color::Rgb(0x12, 0x34, 0x56));
    assert_eq!(theme.input_cursor, Color::Rgb(0x12, 0x34, 0x56));

    for name in ["input_bg", "input_prefix", "input_cursor"] {
        assert!(theme.set_highlight(name, None));
    }
    let defaults = Theme::default();
    assert_eq!(theme.input_bg, defaults.input_bg);
    assert_eq!(theme.input_prefix, defaults.input_prefix);
    assert_eq!(theme.input_cursor, defaults.input_cursor);
}

/// Find the foreground of the rule whose selector is exactly `scope`.
fn code_fg(theme: &Theme, scope: &str) -> Option<SyColor> {
    let sel: syntect::highlighting::ScopeSelectors = scope.parse().unwrap();
    theme
        .code()
        .scopes
        .iter()
        .find(|item| item.scope == sel)
        .and_then(|item| item.style.foreground)
}

#[test]
fn syntax_highlight_rebuilds_code_theme() {
    let mut theme = Theme::default();

    // Defaults land in the built syntect theme.
    assert_eq!(
        theme.code().settings.foreground,
        to_syntect(Color::Rgb(0xD4, 0xD4, 0xD4))
    );
    assert_eq!(code_fg(&theme, "comment"), to_syntect(theme.syntax_comment));

    // set_highlight on a syntax_* group propagates into the code theme.
    assert!(theme.set_highlight("syntax_comment", Some("#123456")));
    assert_eq!(
        code_fg(&theme, "comment"),
        Some(SyColor {
            r: 0x12,
            g: 0x34,
            b: 0x56,
            a: 0xFF
        })
    );

    // Reset restores the default in both the field and the code theme.
    assert!(theme.set_highlight("syntax_comment", None));
    assert_eq!(theme.syntax_comment, Theme::default().syntax_comment);
    assert_eq!(code_fg(&theme, "comment"), to_syntect(theme.syntax_comment));

    // apply_snapshot also rebuilds.
    let snap = crate::config::settings::ThemeSettings {
        syntax_string: Some("#ff00ff".to_string()),
        ..Default::default()
    };
    theme.apply_snapshot(&snap);
    assert_eq!(
        code_fg(&theme, "string"),
        Some(SyColor {
            r: 0xFF,
            g: 0x00,
            b: 0xFF,
            a: 0xFF
        })
    );
}

#[test]
fn runtime_reset_reveals_configured_value_and_keeps_snapshot_layers_separate() {
    let mut theme = Theme::default();
    let snap = crate::config::settings::ThemeSettings {
        palette: crate::config::settings::ThemePaletteSettings {
            fg: Some("#112233".into()),
            bg: Some("#010203".into()),
            ..Default::default()
        },
        ..Default::default()
    };
    theme.apply_snapshot(&snap);
    assert_eq!(theme.palette.fg, Color::Rgb(0x11, 0x22, 0x33));
    assert!(!theme.set_highlight("fg", Some("#ffffff")));
    assert!(theme.set_highlight("user_msg", Some("#ffffff")));
    assert_eq!(theme.user_msg, Color::Rgb(0xff, 0xff, 0xff));
    assert!(theme.set_highlight("user_msg", None));
    assert_eq!(theme.user_msg, Color::Rgb(0x11, 0x22, 0x33));
    assert!(theme.set_highlight("bg", Some("#aabbcc")));
    assert_eq!(theme.palette.bg, Some(Color::Rgb(0xaa, 0xbb, 0xcc)));
    assert!(theme.set_highlight("bg", None));
    assert_eq!(theme.palette.bg, Some(Color::Rgb(1, 2, 3)));
    assert_eq!(
        theme.configured_theme().palette.fg,
        Color::Rgb(0x11, 0x22, 0x33)
    );
    assert!(theme.runtime_overrides().is_empty());
}
#[test]
fn structured_theme_applies_palette_shell_syntax_and_highlights() {
    let mut highlights = std::collections::BTreeMap::new();
    highlights.insert(
        "user_msg".to_string(),
        crate::config::settings::ThemeStyleSpec::Style {
            fg: Some("fg".to_string()),
            bg: Some("selection".to_string()),
        },
    );
    highlights.insert(
        "syntax_keyword".to_string(),
        crate::config::settings::ThemeStyleSpec::Color("accent".to_string()),
    );

    let snap = crate::config::settings::ThemeSettings {
        palette: crate::config::settings::ThemePaletteSettings {
            fg: Some("#111111".to_string()),
            accent: Some("#222222".to_string()),
            error: Some("#333333".to_string()),
            selection: Some("#444444".to_string()),
            ..Default::default()
        },
        shell: crate::config::settings::ThemeShellSettings {
            program: Some("#555555".to_string()),
            ..Default::default()
        },
        syntax: crate::config::settings::ThemeSyntaxSettings {
            function_name: Some("#666666".to_string()),
            ..Default::default()
        },
        highlights,
        ..Default::default()
    };

    let mut theme = Theme::default();
    theme.apply_snapshot(&snap);

    assert_eq!(theme.palette.fg, Color::Rgb(0x11, 0x11, 0x11));
    assert_eq!(theme.approval_danger, Color::Rgb(0x33, 0x33, 0x33));
    assert_eq!(theme.tool_error, Color::Rgb(0x33, 0x33, 0x33));
    assert_eq!(theme.thinking, Color::Rgb(0x22, 0x22, 0x22));
    assert_eq!(theme.user_msg, Color::Rgb(0x11, 0x11, 0x11));
    assert_eq!(theme.user_msg_bg, Color::Rgb(0x44, 0x44, 0x44));
    assert_eq!(theme.shell_program, Color::Rgb(0x55, 0x55, 0x55));
    assert_eq!(theme.syntax_function, Color::Rgb(0x66, 0x66, 0x66));
    assert_eq!(theme.syntax_keyword, Color::Rgb(0x22, 0x22, 0x22));
}

#[test]
fn settings_reload_under_override_exposes_new_configured_value_after_reset() {
    let mut theme = Theme::default();
    let first = crate::config::settings::ThemeSettings {
        input_border: Some("#111111".into()),
        ..Default::default()
    };
    let second = crate::config::settings::ThemeSettings {
        input_border: Some("#222222".into()),
        ..Default::default()
    };

    theme.apply_snapshot(&first);
    assert!(theme.set_highlight("input_border", Some("#abcdef")));
    theme.apply_snapshot(&second);

    assert_eq!(theme.input_border, Color::Rgb(0xab, 0xcd, 0xef));
    assert_eq!(
        theme.configured_theme().input_border,
        Color::Rgb(0x22, 0x22, 0x22)
    );
    assert!(theme.set_highlight("input_border", None));
    assert_eq!(theme.input_border, Color::Rgb(0x22, 0x22, 0x22));
}

#[test]
fn absent_configured_background_is_restored_after_runtime_override() {
    let mut theme = Theme::default();
    theme.apply_snapshot(&Default::default());
    assert_eq!(theme.configured_theme().palette.bg, None);

    assert!(theme.set_highlight("bg", Some("#123456")));
    assert_eq!(theme.palette.bg, Some(Color::Rgb(0x12, 0x34, 0x56)));
    assert!(theme.set_highlight("bg", None));

    assert_eq!(theme.palette.bg, None);
    assert_eq!(theme.configured_theme().palette.bg, None);
}

#[test]
fn runtime_syntax_changes_rebuild_once_and_unrelated_changes_do_not() {
    let mut theme = Theme::default();
    let initial = theme.code_rebuilds();

    assert!(theme.set_highlight("syntax_comment", Some("#123456")));
    assert_eq!(theme.code_rebuilds(), initial + 1);

    let after_set = theme.code_rebuilds();
    assert!(theme.set_highlight("input_border", Some("#654321")));
    assert_eq!(theme.code_rebuilds(), after_set);

    let reloaded = crate::config::settings::ThemeSettings {
        syntax_comment: Some("#010203".into()),
        ..Default::default()
    };
    theme.apply_snapshot(&reloaded);
    assert_eq!(theme.code_rebuilds(), after_set + 1);
    assert_eq!(theme.syntax_comment, Color::Rgb(0x12, 0x34, 0x56));

    assert!(theme.set_highlight("syntax_comment", None));
    assert_eq!(theme.code_rebuilds(), after_set + 2);
    assert_eq!(theme.syntax_comment, Color::Rgb(1, 2, 3));
    assert_eq!(code_fg(&theme, "comment"), to_syntect(theme.syntax_comment));
}

#[test]
fn full_snapshot_and_equivalent_incremental_updates_have_equal_layers() {
    let configured = crate::config::settings::ThemeSettings {
        input_border: Some("#112233".into()),
        syntax_comment: Some("#223344".into()),
        ..Default::default()
    };
    let overrides = std::collections::HashMap::from([
        ("input_border".to_string(), "#abcdef".to_string()),
        ("syntax_comment".to_string(), "#fedcba".to_string()),
    ]);

    let mut snapshot = Theme::default();
    snapshot.apply_resolved_snapshot(&configured, &overrides);

    let mut incremental = Theme::default();
    incremental.apply_snapshot(&configured);
    assert!(incremental.set_highlight("input_border", Some("#abcdef")));
    assert!(incremental.set_highlight("syntax_comment", Some("#fedcba")));

    assert_eq!(snapshot.input_border, incremental.input_border);
    assert_eq!(snapshot.syntax_comment, incremental.syntax_comment);
    assert_eq!(
        snapshot.configured_theme().input_border,
        incremental.configured_theme().input_border
    );
    assert_eq!(
        snapshot.configured_theme().syntax_comment,
        incremental.configured_theme().syntax_comment
    );
    assert_eq!(
        snapshot.runtime_overrides(),
        incremental.runtime_overrides()
    );
    assert_eq!(snapshot.code().settings, incremental.code().settings);
    assert_eq!(snapshot.code().scopes, incremental.code().scopes);

    let reloaded = crate::config::settings::ThemeSettings {
        input_border: Some("#334455".into()),
        syntax_comment: Some("#445566".into()),
        ..Default::default()
    };
    snapshot.apply_resolved_snapshot(&reloaded, &overrides);
    incremental.apply_snapshot(&reloaded);
    assert_eq!(snapshot.input_border, incremental.input_border);
    assert_eq!(snapshot.syntax_comment, incremental.syntax_comment);
    assert_eq!(
        snapshot.configured_theme().input_border,
        incremental.configured_theme().input_border
    );
    assert_eq!(snapshot.code().scopes, incremental.code().scopes);

    snapshot.apply_resolved_snapshot(&reloaded, &Default::default());
    assert!(incremental.set_highlight("input_border", None));
    assert!(incremental.set_highlight("syntax_comment", None));
    assert_eq!(snapshot.input_border, incremental.input_border);
    assert_eq!(snapshot.syntax_comment, incremental.syntax_comment);
    assert_eq!(
        snapshot.runtime_overrides(),
        incremental.runtime_overrides()
    );
    assert_eq!(snapshot.code().settings, incremental.code().settings);
    assert_eq!(snapshot.code().scopes, incremental.code().scopes);
}

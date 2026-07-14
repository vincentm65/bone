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
        Some(to_syntect(Color::Rgb(0xD4, 0xD4, 0xD4)))
    );
    assert_eq!(
        code_fg(&theme, "comment"),
        Some(to_syntect(theme.syntax_comment))
    );

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
    assert_eq!(
        code_fg(&theme, "comment"),
        Some(to_syntect(theme.syntax_comment))
    );

    // apply_snapshot also rebuilds.
    let snap = crate::ext::snapshots::LuaThemeSnapshot {
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
fn structured_theme_applies_palette_shell_syntax_and_highlights() {
    let mut highlights = std::collections::BTreeMap::new();
    highlights.insert(
        "user_msg".to_string(),
        crate::ext::snapshots::LuaStyleSpec::Style {
            fg: Some("fg".to_string()),
            bg: Some("selection".to_string()),
            bold: None,
            italic: None,
            underline: None,
        },
    );
    highlights.insert(
        "syntax_keyword".to_string(),
        crate::ext::snapshots::LuaStyleSpec::Color("accent".to_string()),
    );

    let snap = crate::ext::snapshots::LuaThemeSnapshot {
        palette: crate::ext::snapshots::LuaThemePaletteSnapshot {
            fg: Some("#111111".to_string()),
            accent: Some("#222222".to_string()),
            error: Some("#333333".to_string()),
            selection: Some("#444444".to_string()),
            ..Default::default()
        },
        shell: crate::ext::snapshots::LuaThemeShellSnapshot {
            program: Some("#555555".to_string()),
            ..Default::default()
        },
        syntax: crate::ext::snapshots::LuaThemeSyntaxSnapshot {
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
    assert_eq!(theme.tab_active, Color::Rgb(0x22, 0x22, 0x22));
    assert_eq!(theme.user_msg, Color::Rgb(0x11, 0x11, 0x11));
    assert_eq!(theme.user_msg_bg, Color::Rgb(0x44, 0x44, 0x44));
    assert_eq!(theme.shell_program, Color::Rgb(0x55, 0x55, 0x55));
    assert_eq!(theme.syntax_function, Color::Rgb(0x66, 0x66, 0x66));
    assert_eq!(theme.syntax_keyword, Color::Rgb(0x22, 0x22, 0x22));
}

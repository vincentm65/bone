//! Canonical theme role metadata and backend-neutral color values.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorValue {
    Named(NamedColor),
    Rgb(u8, u8, u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedColor {
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    Gray,
    DarkGray,
    White,
    LightRed,
    LightGreen,
    LightYellow,
    LightBlue,
    LightMagenta,
    LightCyan,
}

pub fn parse_color(input: &str) -> Result<ColorValue, String> {
    let value = input.trim();
    if value.is_empty() {
        return Err("color is empty".into());
    }
    let hex = value.strip_prefix('#').unwrap_or(value);
    let upper = value.to_ascii_uppercase();
    let named = match upper.as_str() {
        "BLACK" => Some(NamedColor::Black),
        "RED" => Some(NamedColor::Red),
        "GREEN" => Some(NamedColor::Green),
        "YELLOW" => Some(NamedColor::Yellow),
        "BLUE" => Some(NamedColor::Blue),
        "MAGENTA" => Some(NamedColor::Magenta),
        "CYAN" => Some(NamedColor::Cyan),
        "GRAY" | "GREY" => Some(NamedColor::Gray),
        "DARKGRAY" | "DARK_GRAY" | "DARKGREY" | "DARK_GREY" => Some(NamedColor::DarkGray),
        "WHITE" => Some(NamedColor::White),
        "LIGHTRED" => Some(NamedColor::LightRed),
        "LIGHTGREEN" => Some(NamedColor::LightGreen),
        "LIGHTYELLOW" => Some(NamedColor::LightYellow),
        "LIGHTBLUE" => Some(NamedColor::LightBlue),
        "LIGHTMAGENTA" => Some(NamedColor::LightMagenta),
        "LIGHTCYAN" => Some(NamedColor::LightCyan),
        _ => None,
    };
    if let Some(color) = named {
        return Ok(ColorValue::Named(color));
    }
    if hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Ok(ColorValue::Rgb(
            u8::from_str_radix(&hex[0..2], 16).unwrap(),
            u8::from_str_radix(&hex[2..4], 16).unwrap(),
            u8::from_str_radix(&hex[4..6], 16).unwrap(),
        ));
    }
    Err(format!(
        "unsupported color {input:?}; expected a named color or RRGGBB"
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleKind {
    Foreground,
    Background,
    Composite,
}

#[derive(Debug, Clone, Copy)]
pub struct RoleSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub kind: RoleKind,
    pub runtime: bool,
    pub syntax: bool,
}

const PALETTE: &[&str] = &[
    "bg",
    "fg",
    "muted",
    "subtle",
    "border",
    "accent",
    "good",
    "warn",
    "error",
    "selection",
];
const UI: &[&str] = &[
    "user_msg",
    "user_msg_bg",
    "status_text",
    "input_border",
    "input_bg",
    "input_prefix",
    "input_cursor",
    "system_msg",
    "approval_safe",
    "approval_danger",
    "tool_call",
    "tool_error",
    "diff_removed",
    "diff_added",
    "thinking",
];
const SHELL: &[&str] = &[
    "shell_program",
    "shell_separator",
    "shell_redirect",
    "shell_flag",
    "shell_string",
    "shell_variable",
    "shell_comment",
    "shell_path",
];
const SYNTAX: &[&str] = &[
    "syntax_text",
    "syntax_comment",
    "syntax_string",
    "syntax_number",
    "syntax_constant",
    "syntax_escape",
    "syntax_regex",
    "syntax_keyword",
    "syntax_keyword_control",
    "syntax_type",
    "syntax_function",
    "syntax_variable",
    "syntax_tag",
    "syntax_attribute",
    "syntax_punctuation",
    "syntax_subtle",
    "syntax_markup",
    "syntax_invalid",
];
const MARKDOWN: &[&str] = &[
    "markdown_marker",
    "markdown_heading",
    "markdown_link",
    "markdown_inline_code",
    "markdown_rule",
    "markdown_table_border",
    "markdown_table_header",
];
const STATS: &[&str] = &["chart", "chart_empty", "heat_low", "heat_high"];

pub fn role(name: &str) -> Option<RoleSpec> {
    let (group, kind, runtime, description) = if PALETTE.contains(&name) {
        (
            PALETTE,
            if name == "bg" {
                RoleKind::Background
            } else {
                RoleKind::Foreground
            },
            name == "bg",
            "palette color",
        )
    } else if UI.contains(&name) {
        (
            UI,
            if name == "user_msg" {
                RoleKind::Composite
            } else if name.ends_with("_bg") {
                RoleKind::Background
            } else {
                RoleKind::Foreground
            },
            true,
            "UI role",
        )
    } else if SHELL.contains(&name) {
        (SHELL, RoleKind::Foreground, true, "shell role")
    } else if SYNTAX.contains(&name) {
        (SYNTAX, RoleKind::Foreground, true, "syntax role")
    } else if MARKDOWN.contains(&name) {
        (MARKDOWN, RoleKind::Foreground, true, "Markdown role")
    } else if STATS.contains(&name) {
        (STATS, RoleKind::Foreground, true, "statistics role")
    } else {
        return None;
    };
    let canonical_name = group.iter().copied().find(|candidate| *candidate == name)?;
    Some(RoleSpec {
        name: canonical_name,
        description,
        kind,
        runtime,
        syntax: std::ptr::eq(group, SYNTAX),
    })
}

pub fn role_names() -> impl Iterator<Item = &'static str> {
    PALETTE
        .iter()
        .chain(UI)
        .chain(SHELL)
        .chain(SYNTAX)
        .chain(MARKDOWN)
        .chain(STATS)
        .copied()
}

/// Generate the exhaustive public role table embedded in the default AGENTS.md.
pub fn role_docs_markdown() -> String {
    let mut output = String::from("| Role | Channel | Runtime |\n|---|---|:---:|\n");
    for name in role_names() {
        let spec = role(name).expect("registered role");
        let channel = match spec.kind {
            RoleKind::Foreground => "fg",
            RoleKind::Background => "bg",
            RoleKind::Composite => "fg + bg",
        };
        output.push_str(&format!(
            "| `{}` | {} | {} |\n",
            spec.name,
            channel,
            if spec.runtime { "yes" } else { "no" }
        ));
    }
    output
}

pub fn palette_name(name: &str) -> bool {
    PALETTE.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_accepts_named_and_rgb_forms_only() {
        assert_eq!(
            parse_color(" dark_gray "),
            Ok(ColorValue::Named(NamedColor::DarkGray))
        );
        assert_eq!(
            parse_color("#12aBc3"),
            Ok(ColorValue::Rgb(0x12, 0xab, 0xc3))
        );
        assert_eq!(parse_color("12aBc3"), Ok(ColorValue::Rgb(0x12, 0xab, 0xc3)));
        assert!(parse_color("#12345").is_err());
        assert!(parse_color("indexed(1)").is_err());
    }

    #[test]
    fn registry_is_complete_unique_and_excludes_removed_role() {
        let names = role_names().collect::<Vec<_>>();
        let unique = names
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(names.len(), 10 + 15 + 8 + 18 + 7 + 4);
        assert_eq!(names.len(), unique.len());
        assert!(role("markdown_heading").is_some());
        assert!(role("tab_active").is_none());
        assert!(!role("fg").unwrap().runtime);
        assert!(role("bg").unwrap().runtime);
    }

    #[test]
    fn checked_in_role_documentation_is_current() {
        let docs = include_str!("../../defaults/AGENTS.md");
        let documented = docs
            .split("<!-- BEGIN GENERATED THEME ROLES -->\n")
            .nth(1)
            .and_then(|tail| tail.split("<!-- END GENERATED THEME ROLES -->").next())
            .expect("generated theme role section");
        assert_eq!(documented.trim(), role_docs_markdown().trim());
    }
}

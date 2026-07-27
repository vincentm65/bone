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

#[derive(Clone, Copy, PartialEq, Eq)]
enum RoleGroup {
    Palette,
    Ui,
    Shell,
    Syntax,
    Markdown,
    Stats,
}

#[derive(Clone, Copy)]
struct RoleEntry {
    name: &'static str,
    group: RoleGroup,
    kind: RoleKind,
}

impl RoleEntry {
    fn spec(self) -> RoleSpec {
        RoleSpec {
            name: self.name,
            description: match self.group {
                RoleGroup::Palette => "palette color",
                RoleGroup::Ui => "UI role",
                RoleGroup::Shell => "shell role",
                RoleGroup::Syntax => "syntax role",
                RoleGroup::Markdown => "Markdown role",
                RoleGroup::Stats => "statistics role",
            },
            kind: self.kind,
            runtime: !matches!(self.group, RoleGroup::Palette) || self.name == "bg",
            syntax: matches!(self.group, RoleGroup::Syntax),
        }
    }
}

const fn entry(name: &'static str, group: RoleGroup, kind: RoleKind) -> RoleEntry {
    RoleEntry { name, group, kind }
}

use RoleGroup::{Markdown, Palette, Shell, Stats, Syntax, Ui};
const FG: RoleKind = RoleKind::Foreground;
const BG: RoleKind = RoleKind::Background;
const COMPOSITE: RoleKind = RoleKind::Composite;

/// Single source of truth for role lookup, validation, iteration, and docs.
const ROLES: &[RoleEntry] = &[
    entry("bg", Palette, BG),
    entry("fg", Palette, FG),
    entry("muted", Palette, FG),
    entry("subtle", Palette, FG),
    entry("border", Palette, FG),
    entry("accent", Palette, FG),
    entry("good", Palette, FG),
    entry("warn", Palette, FG),
    entry("error", Palette, FG),
    entry("selection", Palette, FG),
    entry("user_msg", Ui, COMPOSITE),
    entry("user_msg_bg", Ui, BG),
    entry("status_text", Ui, FG),
    entry("input_border", Ui, FG),
    entry("input_bg", Ui, BG),
    entry("input_prefix", Ui, FG),
    entry("input_cursor", Ui, FG),
    entry("system_msg", Ui, FG),
    entry("approval_safe", Ui, FG),
    entry("approval_danger", Ui, FG),
    entry("tool_call", Ui, FG),
    entry("tool_error", Ui, FG),
    entry("diff_removed", Ui, FG),
    entry("diff_added", Ui, FG),
    entry("thinking", Ui, FG),
    entry("shell_program", Shell, FG),
    entry("shell_separator", Shell, FG),
    entry("shell_redirect", Shell, FG),
    entry("shell_flag", Shell, FG),
    entry("shell_string", Shell, FG),
    entry("shell_variable", Shell, FG),
    entry("shell_comment", Shell, FG),
    entry("shell_path", Shell, FG),
    entry("syntax_text", Syntax, FG),
    entry("syntax_comment", Syntax, FG),
    entry("syntax_string", Syntax, FG),
    entry("syntax_number", Syntax, FG),
    entry("syntax_constant", Syntax, FG),
    entry("syntax_escape", Syntax, FG),
    entry("syntax_regex", Syntax, FG),
    entry("syntax_keyword", Syntax, FG),
    entry("syntax_keyword_control", Syntax, FG),
    entry("syntax_type", Syntax, FG),
    entry("syntax_function", Syntax, FG),
    entry("syntax_variable", Syntax, FG),
    entry("syntax_tag", Syntax, FG),
    entry("syntax_attribute", Syntax, FG),
    entry("syntax_punctuation", Syntax, FG),
    entry("syntax_subtle", Syntax, FG),
    entry("syntax_markup", Syntax, FG),
    entry("syntax_invalid", Syntax, FG),
    entry("markdown_marker", Markdown, FG),
    entry("markdown_heading", Markdown, FG),
    entry("markdown_link", Markdown, FG),
    entry("markdown_inline_code", Markdown, FG),
    entry("markdown_rule", Markdown, FG),
    entry("markdown_table_border", Markdown, FG),
    entry("markdown_table_header", Markdown, FG),
    entry("chart", Stats, FG),
    entry("chart_empty", Stats, FG),
    entry("heat_low", Stats, FG),
    entry("heat_high", Stats, FG),
];

pub fn role(name: &str) -> Option<RoleSpec> {
    ROLES
        .iter()
        .find(|entry| entry.name == name)
        .map(|entry| entry.spec())
}

pub fn role_names() -> impl Iterator<Item = &'static str> {
    ROLES.iter().map(|entry| entry.name)
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
    ROLES
        .iter()
        .any(|entry| entry.group == Palette && entry.name == name)
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

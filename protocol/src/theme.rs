//! Canonical theme role metadata, backend-neutral color values, and the
//! persisted theme settings shape shared by the daemon and every frontend.

use serde::{Deserialize, Serialize};

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
    entry("diff_removed", Ui, COMPOSITE),
    entry("diff_removed_bg", Ui, BG),
    entry("diff_added", Ui, COMPOSITE),
    entry("diff_added_bg", Ui, BG),
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

/// Generate the exhaustive public role table embedded in the default configuration documentation.
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
#[path = "theme_tests.rs"]
mod tests;

// ── Theme (unchanged shape) ──────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemePaletteSettings {
    pub bg: Option<String>,
    pub fg: Option<String>,
    pub muted: Option<String>,
    pub subtle: Option<String>,
    pub border: Option<String>,
    pub accent: Option<String>,
    pub good: Option<String>,
    pub warn: Option<String>,
    pub error: Option<String>,
    pub selection: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeShellSettings {
    pub program: Option<String>,
    pub separator: Option<String>,
    pub redirect: Option<String>,
    pub flag: Option<String>,
    pub string: Option<String>,
    pub variable: Option<String>,
    pub comment: Option<String>,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeSyntaxSettings {
    pub text: Option<String>,
    pub comment: Option<String>,
    pub string: Option<String>,
    pub number: Option<String>,
    pub constant: Option<String>,
    pub escape: Option<String>,
    pub regex: Option<String>,
    pub keyword: Option<String>,
    pub keyword_control: Option<String>,
    #[serde(rename = "type")]
    pub r#type: Option<String>,
    pub function_name: Option<String>,
    pub variable: Option<String>,
    pub tag: Option<String>,
    pub attribute: Option<String>,
    pub punctuation: Option<String>,
    pub subtle: Option<String>,
    pub markup: Option<String>,
    pub invalid: Option<String>,
}

/// A highlight is either a scalar color/reference or an explicit channel object.
/// Objects accept only `fg` for foreground roles, only `bg` for background roles,
/// and both channels for the composite `user_msg` role. Typography modifiers are
/// intentionally not part of the persisted theme schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum ThemeStyleSpec {
    Color(String),
    Style {
        fg: Option<String>,
        bg: Option<String>,
    },
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ThemeSettings {
    /// Selected theme from `lua/themes/<name>.lua` or a plugin-shipped
    /// `lua/plugins/<pkg>/themes/<name>.lua` (a user copy wins). The resolved
    /// fields below are persisted alongside it so frontends never need
    /// filesystem or Lua access.
    pub name: Option<String>,
    pub palette: ThemePaletteSettings,
    pub shell: ThemeShellSettings,
    pub syntax: ThemeSyntaxSettings,
    pub highlights: std::collections::BTreeMap<String, ThemeStyleSpec>,
    pub user_msg: Option<String>,
    pub user_msg_bg: Option<String>,
    pub status_text: Option<String>,
    pub input_border: Option<String>,
    pub system_msg: Option<String>,
    pub approval_safe: Option<String>,
    pub approval_danger: Option<String>,
    pub tool_call: Option<String>,
    pub tool_error: Option<String>,
    /// Diff *text* color for removed (`-`) lines; the band fill is `diff_removed_bg`.
    pub diff_removed: Option<String>,
    /// Diff *text* color for added (`+`) lines; the band fill is `diff_added_bg`.
    pub diff_added: Option<String>,
    /// Band fill behind removed (`-`) diff lines.
    pub diff_removed_bg: Option<String>,
    /// Band fill behind added (`+`) diff lines.
    pub diff_added_bg: Option<String>,
    pub thinking: Option<String>,
    pub markdown_marker: Option<String>,
    pub markdown_heading: Option<String>,
    pub markdown_link: Option<String>,
    pub markdown_inline_code: Option<String>,
    pub markdown_rule: Option<String>,
    pub markdown_table_border: Option<String>,
    pub markdown_table_header: Option<String>,
    pub chart: Option<String>,
    pub chart_empty: Option<String>,
    pub heat_low: Option<String>,
    pub heat_high: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ThemeSettingsInput {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    palette: ThemePaletteSettings,
    #[serde(default)]
    shell: ThemeShellSettings,
    #[serde(default)]
    syntax: ThemeSyntaxSettings,
    #[serde(default)]
    highlights: std::collections::BTreeMap<String, ThemeStyleSpec>,
    user_msg: Option<String>,
    user_msg_bg: Option<String>,
    status_text: Option<String>,
    input_border: Option<String>,
    system_msg: Option<String>,
    approval_safe: Option<String>,
    approval_danger: Option<String>,
    tool_call: Option<String>,
    tool_error: Option<String>,
    shell_program: Option<String>,
    shell_separator: Option<String>,
    shell_redirect: Option<String>,
    shell_flag: Option<String>,
    shell_string: Option<String>,
    shell_variable: Option<String>,
    shell_comment: Option<String>,
    shell_path: Option<String>,
    diff_removed: Option<String>,
    diff_added: Option<String>,
    diff_removed_bg: Option<String>,
    diff_added_bg: Option<String>,
    thinking: Option<String>,
    markdown_marker: Option<String>,
    markdown_heading: Option<String>,
    markdown_link: Option<String>,
    markdown_inline_code: Option<String>,
    markdown_rule: Option<String>,
    markdown_table_border: Option<String>,
    markdown_table_header: Option<String>,
    chart: Option<String>,
    chart_empty: Option<String>,
    heat_low: Option<String>,
    heat_high: Option<String>,
    syntax_text: Option<String>,
    syntax_comment: Option<String>,
    syntax_string: Option<String>,
    syntax_number: Option<String>,
    syntax_constant: Option<String>,
    syntax_escape: Option<String>,
    syntax_regex: Option<String>,
    syntax_keyword: Option<String>,
    syntax_keyword_control: Option<String>,
    syntax_type: Option<String>,
    syntax_function: Option<String>,
    syntax_variable: Option<String>,
    syntax_tag: Option<String>,
    syntax_attribute: Option<String>,
    syntax_punctuation: Option<String>,
    syntax_subtle: Option<String>,
    syntax_markup: Option<String>,
    syntax_invalid: Option<String>,
}

impl<'de> Deserialize<'de> for ThemeSettings {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let mut input = ThemeSettingsInput::deserialize(deserializer)?;

        // Legacy flat values historically applied after structured values, so
        // they continue to win when both spellings are present.
        input.shell.program = input.shell_program.or(input.shell.program);
        input.shell.separator = input.shell_separator.or(input.shell.separator);
        input.shell.redirect = input.shell_redirect.or(input.shell.redirect);
        input.shell.flag = input.shell_flag.or(input.shell.flag);
        input.shell.string = input.shell_string.or(input.shell.string);
        input.shell.variable = input.shell_variable.or(input.shell.variable);
        input.shell.comment = input.shell_comment.or(input.shell.comment);
        input.shell.path = input.shell_path.or(input.shell.path);
        input.syntax.text = input.syntax_text.or(input.syntax.text);
        input.syntax.comment = input.syntax_comment.or(input.syntax.comment);
        input.syntax.string = input.syntax_string.or(input.syntax.string);
        input.syntax.number = input.syntax_number.or(input.syntax.number);
        input.syntax.constant = input.syntax_constant.or(input.syntax.constant);
        input.syntax.escape = input.syntax_escape.or(input.syntax.escape);
        input.syntax.regex = input.syntax_regex.or(input.syntax.regex);
        input.syntax.keyword = input.syntax_keyword.or(input.syntax.keyword);
        input.syntax.keyword_control = input
            .syntax_keyword_control
            .or(input.syntax.keyword_control);
        input.syntax.r#type = input.syntax_type.or(input.syntax.r#type);
        input.syntax.function_name = input.syntax_function.or(input.syntax.function_name);
        input.syntax.variable = input.syntax_variable.or(input.syntax.variable);
        input.syntax.tag = input.syntax_tag.or(input.syntax.tag);
        input.syntax.attribute = input.syntax_attribute.or(input.syntax.attribute);
        input.syntax.punctuation = input.syntax_punctuation.or(input.syntax.punctuation);
        input.syntax.subtle = input.syntax_subtle.or(input.syntax.subtle);
        input.syntax.markup = input.syntax_markup.or(input.syntax.markup);
        input.syntax.invalid = input.syntax_invalid.or(input.syntax.invalid);

        Ok(Self {
            name: input.name,
            palette: input.palette,
            shell: input.shell,
            syntax: input.syntax,
            highlights: input.highlights,
            user_msg: input.user_msg,
            user_msg_bg: input.user_msg_bg,
            status_text: input.status_text,
            input_border: input.input_border,
            system_msg: input.system_msg,
            approval_safe: input.approval_safe,
            approval_danger: input.approval_danger,
            tool_call: input.tool_call,
            tool_error: input.tool_error,
            diff_removed: input.diff_removed,
            diff_added: input.diff_added,
            diff_removed_bg: input.diff_removed_bg,
            diff_added_bg: input.diff_added_bg,
            thinking: input.thinking,
            markdown_marker: input.markdown_marker,
            markdown_heading: input.markdown_heading,
            markdown_link: input.markdown_link,
            markdown_inline_code: input.markdown_inline_code,
            markdown_rule: input.markdown_rule,
            markdown_table_border: input.markdown_table_border,
            markdown_table_header: input.markdown_table_header,
            chart: input.chart,
            chart_empty: input.chart_empty,
            heat_low: input.heat_low,
            heat_high: input.heat_high,
        })
    }
}

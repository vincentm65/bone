//! Highlight groups.
//!
//! Every style the TUI draws with comes from a named group (`ToolName`,
//! `DiffAdd`, ...). Rust ships a 16-color fallback for each; colorschemes
//! and `bone.hl.set` in Lua override them.

use std::collections::HashMap;

use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone)]
pub struct Theme {
    groups: HashMap<String, Style>,
}

/// `(group, fallback)`. The fallback only uses the 16 ANSI colors so it
/// works on any terminal.
fn fallback() -> Vec<(&'static str, Style)> {
    let fg = |c| Style::default().fg(c);
    let bold = Style::default().add_modifier(Modifier::BOLD);
    vec![
        ("Normal", Style::default()),
        ("Dim", fg(Color::DarkGray)),
        ("Accent", fg(Color::Blue).add_modifier(Modifier::BOLD)),
        ("UserPrompt", fg(Color::Blue).add_modifier(Modifier::BOLD)),
        ("UserMessage", bold),
        (
            "Reasoning",
            fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
        ),
        ("ToolName", Style::default()),
        ("ToolArgs", fg(Color::Gray)),
        ("ToolPath", fg(Color::Cyan)),
        ("ToolSummary", fg(Color::DarkGray)),
        ("ToolOutput", fg(Color::Gray)),
        ("ToolGutter", fg(Color::DarkGray)),
        ("ToolRunning", fg(Color::DarkGray)),
        ("ToolError", fg(Color::Red)),
        ("DiffAdd", fg(Color::Green)),
        ("DiffDelete", fg(Color::Red)),
        ("ShellProgram", fg(Color::Green)),
        ("ShellPath", fg(Color::Cyan)),
        ("ShellFlag", fg(Color::Blue)),
        ("ShellString", fg(Color::Green)),
        ("ShellVariable", fg(Color::Magenta)),
        ("ShellComment", fg(Color::DarkGray)),
        ("ShellOperator", fg(Color::Gray)),
        ("MdHeading", fg(Color::Blue).add_modifier(Modifier::BOLD)),
        ("MdBold", bold),
        ("MdItalic", Style::default().add_modifier(Modifier::ITALIC)),
        ("MdCode", fg(Color::Yellow)),
        ("MdCodeBlock", fg(Color::Gray)),
        (
            "MdQuote",
            fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
        ),
        ("MdBullet", fg(Color::Blue)),
        ("MdLink", fg(Color::Cyan).add_modifier(Modifier::UNDERLINED)),
        ("Notice", fg(Color::Yellow)),
        ("ErrorMsg", fg(Color::Red)),
        ("WarningMsg", fg(Color::Yellow)),
        ("WinSeparator", fg(Color::DarkGray)),
        ("StatusLine", Style::default()),
        ("StatusLineDim", fg(Color::DarkGray)),
        (
            "Selection",
            Style::default().add_modifier(Modifier::REVERSED),
        ),
        ("Placeholder", fg(Color::DarkGray)),
        ("PopupBorder", fg(Color::Yellow)),
        ("PopupTitle", fg(Color::Yellow).add_modifier(Modifier::BOLD)),
    ]
}

impl Default for Theme {
    fn default() -> Self {
        Theme {
            groups: fallback()
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect(),
        }
    }
}

impl Theme {
    /// A group's style. Unknown groups fall back to `Normal`.
    pub fn hl(&self, name: &str) -> Style {
        self.groups
            .get(name)
            .or_else(|| self.groups.get("Normal"))
            .copied()
            .unwrap_or_default()
    }

    pub fn set(&mut self, name: &str, style: Style) {
        self.groups.insert(name.to_owned(), style);
    }

    pub fn names(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.groups.keys().map(String::as_str).collect();
        v.sort_unstable();
        v
    }

    /// Back to the fallback colors (before a colorscheme loads).
    pub fn reset(&mut self) {
        *self = Theme::default();
    }
}

/// `#rrggbb`, an ANSI name (`red`, `darkgray`, `lightblue`...), `reset`, or a
/// 0-255 palette index.
pub fn parse_color(s: &str) -> Result<Color, String> {
    if let Some(hex) = s.strip_prefix('#') {
        if hex.len() == 6
            && let Ok(v) = u32::from_str_radix(hex, 16)
        {
            return Ok(Color::Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8));
        }
        return Err(format!("bad color {s:?}: use #rrggbb"));
    }
    if let Ok(n) = s.parse::<u8>() {
        return Ok(Color::Indexed(n));
    }
    let c = match s.to_ascii_lowercase().replace(['_', '-', ' '], "").as_str() {
        "reset" | "none" | "default" => Color::Reset,
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "gray" | "grey" => Color::Gray,
        "darkgray" | "darkgrey" => Color::DarkGray,
        "lightred" => Color::LightRed,
        "lightgreen" => Color::LightGreen,
        "lightyellow" => Color::LightYellow,
        "lightblue" => Color::LightBlue,
        "lightmagenta" => Color::LightMagenta,
        "lightcyan" => Color::LightCyan,
        "white" => Color::White,
        _ => return Err(format!("unknown color {s:?}")),
    };
    Ok(c)
}

/// The Lua-facing description of a style: fg, bg and boolean attributes.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct StyleSpec {
    pub fg: Option<String>,
    pub bg: Option<String>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub reverse: bool,
    pub dim: bool,
    /// Another group to start from.
    pub link: Option<String>,
}

impl StyleSpec {
    pub fn to_style(&self, theme: &Theme) -> Result<Style, String> {
        let mut s = match &self.link {
            Some(g) => theme.hl(g),
            None => Style::default(),
        };
        if let Some(c) = &self.fg {
            s = s.fg(parse_color(c)?);
        }
        if let Some(c) = &self.bg {
            s = s.bg(parse_color(c)?);
        }
        for (on, m) in [
            (self.bold, Modifier::BOLD),
            (self.italic, Modifier::ITALIC),
            (self.underline, Modifier::UNDERLINED),
            (self.reverse, Modifier::REVERSED),
            (self.dim, Modifier::DIM),
        ] {
            if on {
                s = s.add_modifier(m);
            }
        }
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_and_specs() {
        assert_eq!(
            parse_color("#7aa2f7").unwrap(),
            Color::Rgb(0x7a, 0xa2, 0xf7)
        );
        assert_eq!(parse_color("Dark_Gray").unwrap(), Color::DarkGray);
        assert_eq!(parse_color("236").unwrap(), Color::Indexed(236));
        assert!(parse_color("#12").is_err() && parse_color("mauve").is_err());

        let mut t = Theme::default();
        let spec = StyleSpec {
            link: Some("ToolError".into()),
            bold: true,
            ..Default::default()
        };
        let s = spec.to_style(&t).unwrap();
        assert_eq!(s.fg, Some(Color::Red));
        assert!(s.add_modifier.contains(Modifier::BOLD));
        t.set("Mine", s);
        assert_eq!(t.hl("Mine"), s);
        assert_eq!(t.hl("Unknown"), t.hl("Normal"));
    }
}

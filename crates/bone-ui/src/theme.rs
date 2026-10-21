//! Highlight groups.
//!
//! Named styles resolve from overrides, then the current colorscheme, then
//! built-in ANSI fallbacks. Loading a scheme replaces only its layer. Links
//! are followed on every lookup, so they track the current layers; a
//! self-link starts from the layer below.

use std::collections::{BTreeSet, HashMap};
use std::sync::LazyLock;

use crate::style::{Color, Modifier, Style};

#[derive(Debug, Clone, Default)]
pub struct Theme {
    /// In lookup order: overrides, then the colorscheme.
    layers: [Layer; 2],
    /// A colorscheme file is running: `set` and `reset` touch its layer.
    loading: bool,
}

type Layer = HashMap<String, Group>;

/// The colorscheme layer from before a load, put back if the load fails.
pub struct SavedScheme(Layer);

/// One layer's definition of a group: `style` on top of the `link` group.
#[derive(Debug, Clone)]
struct Group {
    link: Option<String>,
    style: Style,
}

/// Links deeper than this are a cycle.
const MAX_DEPTH: usize = 32;

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
        ("ShellVariable", fg(Color::Cyan)),
        ("ShellComment", fg(Color::DarkGray)),
        ("ShellOperator", fg(Color::Gray)),
        ("MdHeading", bold),
        ("MdBold", bold),
        ("MdItalic", Style::default().add_modifier(Modifier::ITALIC)),
        ("MdCode", fg(Color::Cyan)),
        ("MdCodeBlock", fg(Color::Gray)),
        (
            "MdQuote",
            fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
        ),
        ("MdBullet", Style::default()),
        ("MdLink", fg(Color::Cyan).add_modifier(Modifier::UNDERLINED)),
        ("MdTable", Style::default()),
        ("MdTableHeader", bold),
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
        ("PopupBorder", fg(Color::DarkGray)),
        ("PopupTitle", fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        ("PanelTitle", bold),
        // The settings pages (/config, /catalog, /setup): attributes only,
        // so they follow the terminal's own colors.
        ("SettingsTitle", bold),
        ("SettingsTab", Style::default().add_modifier(Modifier::DIM)),
        (
            "SettingsTabCurrent",
            Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        ),
        ("SettingsSelected", bold),
        (
            "SettingsMuted",
            Style::default().add_modifier(Modifier::DIM),
        ),
        ("SettingsKey", bold),
        (
            "SettingsNote",
            Style::default().add_modifier(Modifier::DIM | Modifier::ITALIC),
        ),
        (
            "PanelTitleFocus",
            Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
        ),
    ]
}

static FALLBACK: LazyLock<HashMap<String, Style>> = LazyLock::new(|| {
    fallback()
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect()
});

impl Theme {
    /// A group's style. Unknown groups, and links that lead nowhere, are
    /// `Normal`.
    pub fn hl(&self, name: &str) -> Style {
        self.resolve(name, 0, 0)
            .or_else(|| self.resolve("Normal", 0, 0))
            .unwrap_or_default()
    }

    pub fn has(&self, name: &str) -> bool {
        FALLBACK.contains_key(name) || self.layers.iter().any(|l| l.contains_key(name))
    }

    /// Define a group from Lua: in the colorscheme's layer while one loads,
    /// else in the overrides.
    pub fn set(&mut self, name: &str, spec: &StyleSpec) -> Result<(), String> {
        let group = spec.to_group()?;
        self.layers[usize::from(self.loading)].insert(name.to_owned(), group);
        Ok(())
    }

    /// Define an override: `style` on top of the `link` group.
    pub fn set_linked(&mut self, name: &str, link: &str, style: Style) {
        let link = Some(link.to_owned());
        self.layers[0].insert(name.to_owned(), Group { link, style });
    }

    pub fn names(&self) -> Vec<&str> {
        let names: BTreeSet<&str> = FALLBACK
            .keys()
            .chain(self.layers.iter().flat_map(HashMap::keys))
            .map(String::as_str)
            .collect();
        names.into_iter().collect()
    }

    /// A colorscheme starts loading into an empty layer. `None` when it is
    /// nested in another load, which it adds to.
    pub fn begin_scheme(&mut self) -> Option<SavedScheme> {
        if self.loading {
            return None;
        }
        self.loading = true;
        Some(SavedScheme(std::mem::take(&mut self.layers[1])))
    }

    /// The load from `begin_scheme` ends; a failed one puts the previous
    /// colorscheme back.
    pub fn end_scheme(&mut self, saved: Option<SavedScheme>, ok: bool) {
        if let Some(SavedScheme(prev)) = saved {
            self.loading = false;
            if !ok {
                self.layers[1] = prev;
            }
        }
    }

    /// Back to the fallback colors: in a colorscheme file, its own groups
    /// go; anywhere else, every group set from Lua.
    pub fn reset(&mut self) {
        self.layers[usize::from(self.loading)..]
            .iter_mut()
            .for_each(HashMap::clear);
    }

    /// `name` from layer `from` down (0 overrides, 1 scheme, 2 fallback);
    /// `None` when it is undefined or its links never end.
    fn resolve(&self, name: &str, from: usize, depth: usize) -> Option<Style> {
        if depth > MAX_DEPTH {
            return None;
        }
        for (i, layer) in self.layers.iter().enumerate().skip(from) {
            if let Some(g) = layer.get(name) {
                let base = match &g.link {
                    None => Style::default(),
                    // A link to its own name is the layer below's version.
                    Some(l) if l == name => self.resolve(name, i + 1, depth + 1)?,
                    Some(l) => self.resolve(l, 0, depth + 1)?,
                };
                return Some(base.patch(g.style));
            }
        }
        FALLBACK.get(name).copied()
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
    pub strikethrough: bool,
    pub reverse: bool,
    pub dim: bool,
    /// Another group to start from.
    pub link: Option<String>,
}

impl StyleSpec {
    fn to_group(&self) -> Result<Group, String> {
        let mut s = Style::default();
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
            (self.strikethrough, Modifier::CROSSED_OUT),
            (self.reverse, Modifier::REVERSED),
            (self.dim, Modifier::DIM),
        ] {
            if on {
                s = s.add_modifier(m);
            }
        }
        Ok(Group {
            link: self.link.clone(),
            style: s,
        })
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
            strikethrough: true,
            ..Default::default()
        };
        t.set("Mine", &spec).unwrap();
        let s = t.hl("Mine");
        assert_eq!(s.fg, Some(Color::Red));
        assert!(s.add_modifier.contains(Modifier::BOLD));
        assert!(s.add_modifier.contains(Modifier::CROSSED_OUT));
        assert_eq!(t.hl("Unknown"), t.hl("Normal"));
        let bad = StyleSpec {
            fg: Some("mauve".into()),
            ..Default::default()
        };
        assert!(t.set("Bad", &bad).is_err() && !t.has("Bad"));
    }

    fn spec(link: Option<&str>, fg: Option<&str>, bold: bool) -> StyleSpec {
        StyleSpec {
            link: link.map(str::to_owned),
            fg: fg.map(str::to_owned),
            bold,
            ..Default::default()
        }
    }

    /// `set` as a colorscheme file would.
    fn scheme(t: &mut Theme, name: &str, spec: StyleSpec) {
        t.loading = true;
        t.set(name, &spec).unwrap();
        t.loading = false;
    }

    #[test]
    fn layers_and_live_links() {
        let mut t = Theme::default();
        scheme(&mut t, "Dim", spec(None, Some("#111111"), false));
        scheme(&mut t, "SchemeOnly", spec(None, Some("red"), false));
        t.set("Done", &spec(Some("Dim"), None, true)).unwrap();
        t.set("ToolPath", &spec(None, Some("#222222"), false))
            .unwrap();
        assert_eq!(t.hl("Done").fg, Some(Color::Rgb(0x11, 0x11, 0x11)));

        // A new scheme: overrides stay, links follow it, its old groups go.
        let saved = t.begin_scheme();
        t.end_scheme(saved, true);
        assert!(!t.has("SchemeOnly"));
        assert_eq!(t.hl("Done").fg, Some(Color::DarkGray));
        assert!(t.hl("Done").add_modifier.contains(Modifier::BOLD));
        assert_eq!(t.hl("ToolPath").fg, Some(Color::Rgb(0x22, 0x22, 0x22)));

        // The top layer's definition replaces the lower one, no merging.
        scheme(&mut t, "Accent", spec(None, Some("blue"), true));
        t.set("Accent", &spec(None, None, false)).unwrap();
        assert_eq!(t.hl("Accent"), Style::default());

        t.reset();
        assert!(!t.has("Done"));
        assert_eq!(t.hl("ToolPath").fg, Some(Color::Cyan));
    }

    #[test]
    fn self_links_and_cycles() {
        let mut t = Theme::default();
        scheme(&mut t, "ToolPath", spec(None, Some("#333333"), false));
        t.set("ToolPath", &spec(Some("ToolPath"), None, true))
            .unwrap();
        let s = t.hl("ToolPath");
        assert_eq!(s.fg, Some(Color::Rgb(0x33, 0x33, 0x33)));
        assert!(s.add_modifier.contains(Modifier::BOLD));
        // A scheme's self-link is the fallback's version.
        scheme(&mut t, "Dim", spec(Some("Dim"), None, true));
        assert_eq!(t.hl("Dim").fg, Some(Color::DarkGray));

        // A missing link target is Normal; a cycle is Normal.
        t.set("Normal", &spec(None, Some("white"), false)).unwrap();
        t.set("Lost", &spec(Some("Nowhere"), None, false)).unwrap();
        assert_eq!(t.hl("Lost").fg, Some(Color::White));
        t.set("A", &spec(Some("B"), None, false)).unwrap();
        t.set("B", &spec(Some("A"), None, false)).unwrap();
        assert_eq!(t.hl("A").fg, Some(Color::White));
        // A cycle through Normal is the fallback Normal.
        t.set("Normal", &spec(Some("A"), None, false)).unwrap();
        assert_eq!(t.hl("Normal"), Style::default());
        assert_eq!(t.hl("A"), Style::default());
    }

    #[test]
    fn linked_overrides_follow_their_base() {
        let mut t = Theme::default();
        scheme(&mut t, "Normal", spec(None, Some("#444444"), false));
        t.set_linked(
            "@term/2/-/",
            "Normal",
            Style::default().bg(Color::Indexed(2)),
        );
        assert_eq!(t.hl("@term/2/-/").fg, Some(Color::Rgb(0x44, 0x44, 0x44)));
        let saved = t.begin_scheme();
        t.end_scheme(saved, true);
        let s = t.hl("@term/2/-/");
        assert_eq!((s.fg, s.bg), (None, Some(Color::Indexed(2))));
    }
}

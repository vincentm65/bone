//! Highlight groups (from `bone-ui`), resolved to ratatui styles for
//! drawing.

use std::ops::{Deref, DerefMut};

use bone_ui::style::{Color, Modifier, Style};
pub use bone_ui::theme::StyleSpec;

#[derive(Debug, Clone, Default)]
pub struct Theme(bone_ui::theme::Theme);

impl Theme {
    /// A group's style, for ratatui.
    pub fn hl(&self, name: &str) -> ratatui::style::Style {
        style(self.0.hl(name))
    }
}

impl Deref for Theme {
    type Target = bone_ui::theme::Theme;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Theme {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

pub fn style(s: Style) -> ratatui::style::Style {
    ratatui::style::Style {
        fg: s.fg.map(color),
        bg: s.bg.map(color),
        add_modifier: modifier(s.add_modifier),
        sub_modifier: modifier(s.sub_modifier),
        ..Default::default()
    }
}

fn modifier(m: Modifier) -> ratatui::style::Modifier {
    use ratatui::style::Modifier as R;
    [
        (Modifier::BOLD, R::BOLD),
        (Modifier::DIM, R::DIM),
        (Modifier::ITALIC, R::ITALIC),
        (Modifier::UNDERLINED, R::UNDERLINED),
        (Modifier::SLOW_BLINK, R::SLOW_BLINK),
        (Modifier::RAPID_BLINK, R::RAPID_BLINK),
        (Modifier::REVERSED, R::REVERSED),
        (Modifier::HIDDEN, R::HIDDEN),
        (Modifier::CROSSED_OUT, R::CROSSED_OUT),
    ]
    .into_iter()
    .filter(|(ours, _)| m.contains(*ours))
    .fold(R::empty(), |acc, (_, theirs)| acc | theirs)
}

pub fn color(c: Color) -> ratatui::style::Color {
    use ratatui::style::Color as R;
    match c {
        Color::Reset => R::Reset,
        Color::Black => R::Black,
        Color::Red => R::Red,
        Color::Green => R::Green,
        Color::Yellow => R::Yellow,
        Color::Blue => R::Blue,
        Color::Magenta => R::Magenta,
        Color::Cyan => R::Cyan,
        Color::Gray => R::Gray,
        Color::DarkGray => R::DarkGray,
        Color::LightRed => R::LightRed,
        Color::LightGreen => R::LightGreen,
        Color::LightYellow => R::LightYellow,
        Color::LightBlue => R::LightBlue,
        Color::LightMagenta => R::LightMagenta,
        Color::LightCyan => R::LightCyan,
        Color::White => R::White,
        Color::Rgb(r, g, b) => R::Rgb(r, g, b),
        Color::Indexed(n) => R::Indexed(n),
    }
}

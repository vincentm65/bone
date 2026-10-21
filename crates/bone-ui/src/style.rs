//! Colors and text styles, independent of any renderer. They mirror the
//! terminal model (16 ANSI colors, a 256-color palette, RGB, and attribute
//! flags); a frontend converts them for drawing.

use bitflags::bitflags;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Color {
    /// The frontend's default color.
    Reset,
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    Gray,
    DarkGray,
    LightRed,
    LightGreen,
    LightYellow,
    LightBlue,
    LightMagenta,
    LightCyan,
    White,
    Rgb(u8, u8, u8),
    /// A 0-255 palette index.
    Indexed(u8),
}

bitflags! {
    #[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct Modifier: u16 {
        const BOLD = 1;
        const DIM = 1 << 1;
        const ITALIC = 1 << 2;
        const UNDERLINED = 1 << 3;
        const SLOW_BLINK = 1 << 4;
        const RAPID_BLINK = 1 << 5;
        const REVERSED = 1 << 6;
        const HIDDEN = 1 << 7;
        const CROSSED_OUT = 1 << 8;
    }
}

/// Colors and attributes to apply on top of whatever is underneath: `None`
/// colors and attributes in neither set are left as they are.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Style {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub add_modifier: Modifier,
    pub sub_modifier: Modifier,
}

impl Style {
    pub const fn fg(mut self, color: Color) -> Self {
        self.fg = Some(color);
        self
    }

    pub const fn bg(mut self, color: Color) -> Self {
        self.bg = Some(color);
        self
    }

    pub const fn add_modifier(mut self, m: Modifier) -> Self {
        self.sub_modifier = self.sub_modifier.difference(m);
        self.add_modifier = self.add_modifier.union(m);
        self
    }

    pub const fn remove_modifier(mut self, m: Modifier) -> Self {
        self.add_modifier = self.add_modifier.difference(m);
        self.sub_modifier = self.sub_modifier.union(m);
        self
    }

    /// `other` applied on top of this style.
    pub fn patch(mut self, other: Style) -> Self {
        self.fg = other.fg.or(self.fg);
        self.bg = other.bg.or(self.bg);
        self.add_modifier.remove(other.sub_modifier);
        self.add_modifier.insert(other.add_modifier);
        self.sub_modifier.remove(other.add_modifier);
        self.sub_modifier.insert(other.sub_modifier);
        self
    }
}

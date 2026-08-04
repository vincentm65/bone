//! Shared color parsing for the TUI.
//!
//! Moved from `ext::color` to keep core free of ratatui.

use ratatui::style::Color;

/// Parse a color string into a ratatui Color using the canonical backend-neutral
/// theme parser. Conversion to ratatui is deliberately kept at this boundary.
pub fn parse_color(s: &str) -> Option<Color> {
    use bone_core::config::theme::{ColorValue, NamedColor};

    match bone_core::config::theme::parse_color(s).ok()? {
        ColorValue::Rgb(r, g, b) => Some(Color::Rgb(r, g, b)),
        ColorValue::Named(color) => Some(match color {
            NamedColor::Black => Color::Black,
            NamedColor::Red => Color::Red,
            NamedColor::Green => Color::Green,
            NamedColor::Yellow => Color::Yellow,
            NamedColor::Blue => Color::Blue,
            NamedColor::Magenta => Color::Magenta,
            NamedColor::Cyan => Color::Cyan,
            NamedColor::Gray => Color::Gray,
            NamedColor::DarkGray => Color::DarkGray,
            NamedColor::White => Color::White,
            NamedColor::LightRed => Color::LightRed,
            NamedColor::LightGreen => Color::LightGreen,
            NamedColor::LightYellow => Color::LightYellow,
            NamedColor::LightBlue => Color::LightBlue,
            NamedColor::LightMagenta => Color::LightMagenta,
            NamedColor::LightCyan => Color::LightCyan,
        }),
    }
}

/// Convert terminal-independent RGB and named ANSI colors to concrete RGB.
/// Indexed colors and `Reset` depend on terminal state, so callers must handle
/// them explicitly instead of silently inventing a replacement color.
pub fn color_to_rgb(color: Color) -> Option<(u8, u8, u8)> {
    Some(match color {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Black => (0x00, 0x00, 0x00),
        Color::Red => (0xCD, 0x31, 0x31),
        Color::Green => (0x0D, 0xBC, 0x79),
        Color::Yellow => (0xE5, 0xE5, 0x10),
        Color::Blue => (0x24, 0x72, 0xC8),
        Color::Magenta => (0xBC, 0x3F, 0xBC),
        Color::Cyan => (0x11, 0xA8, 0xCD),
        Color::Gray => (0xC0, 0xC0, 0xC0),
        Color::DarkGray => (0x80, 0x80, 0x80),
        Color::LightRed => (0xF1, 0x4C, 0x4C),
        Color::LightGreen => (0x23, 0xD1, 0x8B),
        Color::LightYellow => (0xF5, 0xF5, 0x43),
        Color::LightBlue => (0x3B, 0x8E, 0xEA),
        Color::LightMagenta => (0xD6, 0x70, 0xD6),
        Color::LightCyan => (0x29, 0xB8, 0xDB),
        Color::White => (0xFF, 0xFF, 0xFF),
        Color::Indexed(_) | Color::Reset => return None,
    })
}

#[cfg(test)]
#[path = "color_tests.rs"]
mod tests;

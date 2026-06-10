//! Shared color parsing for Lua tool output and theme snapshots.

use ratatui::style::Color;

/// Parse a color string into a ratatui Color.
///
/// Supports:
/// - Named colors (case-insensitive): black, red, green, yellow, blue, magenta,
///   cyan, gray/grey, dark_gray/dark_grey, white, lightred, lightgreen,
///   lightyellow, lightblue, lightmagenta, lightcyan.
/// - Hex colors: `#RRGGBB` or `RRGGBB`.
pub fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim();
    let s = s.strip_prefix('#').unwrap_or(s);
    let s = s.to_ascii_uppercase();
    match s.as_str() {
        "WHITE" => Some(Color::White),
        "BLACK" => Some(Color::Black),
        "RED" => Some(Color::Red),
        "GREEN" => Some(Color::Green),
        "YELLOW" => Some(Color::Yellow),
        "BLUE" => Some(Color::Blue),
        "MAGENTA" => Some(Color::Magenta),
        "CYAN" => Some(Color::Cyan),
        "GRAY" | "GREY" => Some(Color::Gray),
        "DARKGRAY" | "DARK_GRAY" | "DARKGREY" | "DARK_GREY" => Some(Color::DarkGray),
        "LIGHTRED" => Some(Color::LightRed),
        "LIGHTGREEN" => Some(Color::LightGreen),
        "LIGHTYELLOW" => Some(Color::LightYellow),
        "LIGHTBLUE" => Some(Color::LightBlue),
        "LIGHTMAGENTA" => Some(Color::LightMagenta),
        "LIGHTCYAN" => Some(Color::LightCyan),
        _ => {
            if s.len() == 6 {
                let r = u8::from_str_radix(&s[0..2], 16).ok()?;
                let g = u8::from_str_radix(&s[2..4], 16).ok()?;
                let b = u8::from_str_radix(&s[4..6], 16).ok()?;
                Some(Color::Rgb(r, g, b))
            } else {
                None
            }
        }
    }
}

//! Native mirror of the daemon's resolved `ThemeSettings`
//! (`core/src/config/settings.rs`). Only the fields the desktop UI renders are
//! mirrored; unknown daemon fields are ignored so the wire payload keeps
//! evolving without a protocol change.
//!
//! Color references are resolved by [`resolve_color`]: a named palette role
//! (`fg`, `accent`, `muted`, …), a named ANSI color (`red`, `lightgreen`, …)
//! matching the TUI's `color_to_rgb` mapping, or a `#rgb`/`#rrggbb`/`#rrggbbaa`
//! hex string ([`parse_color`]). [`ThemeSettings::colors`] pre-resolves every
//! render role into a [`ThemeColors`] so the transcript and Markdown renderers
//! can read concrete colors without re-parsing each frame.

use eframe::egui;

const CJK_FALLBACKS: &[&str] = &[
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/wenquanyi/wqy-microhei/wqy-microhei.ttc",
    "/System/Library/Fonts/PingFang.ttc",
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
    "C:\\Windows\\Fonts\\msyh.ttc",
    "C:\\Windows\\Fonts\\YuGothM.ttc",
];

/// Embedded fonts keep the desktop consistent across machines; egui's original
/// fallback chain remains available for symbols and characters outside Latin.
///
/// Inter/JetBrains Mono cover text but not the symbol ranges core UI uses
/// (braille spinner frames, box drawing, `✻`/`⧗`/`◑`); Adwaita Mono is
/// appended as the last fallback in both families to fill those gaps.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    for (name, bytes) in [
        (
            "Inter",
            include_bytes!("../assets/fonts/Inter-Regular.ttf").as_slice(),
        ),
        (
            "Inter SemiBold",
            include_bytes!("../assets/fonts/Inter-SemiBold.ttf").as_slice(),
        ),
        (
            "JetBrains Mono",
            include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf").as_slice(),
        ),
        (
            "Adwaita Mono",
            include_bytes!("../assets/fonts/AdwaitaMono-Regular.ttf").as_slice(),
        ),
    ] {
        fonts
            .font_data
            .insert(name.into(), egui::FontData::from_static(bytes).into());
    }
    let proportional = fonts
        .families
        .get_mut(&egui::FontFamily::Proportional)
        .unwrap();
    proportional.insert(0, "Inter".into());
    proportional.push("Adwaita Mono".into());
    let mut semibold = proportional.clone();
    semibold.insert(0, "Inter SemiBold".into());
    fonts
        .families
        .insert(egui::FontFamily::Name("semibold".into()), semibold);
    let monospace = fonts
        .families
        .get_mut(&egui::FontFamily::Monospace)
        .unwrap();
    monospace.insert(0, "JetBrains Mono".into());
    // Before egui's built-ins: Adwaita Mono is a monospace face with the same
    // advance width, so fallback glyphs stay on the terminal cell grid.
    monospace.insert(1, "Adwaita Mono".into());
    // The bundled fonts have no CJK glyphs; fall back to a system font when one
    // is installed.
    if let Some(bytes) = CJK_FALLBACKS
        .iter()
        .find_map(|path| std::fs::read(path).ok())
    {
        fonts
            .font_data
            .insert("cjk".into(), egui::FontData::from_owned(bytes).into());
        for family in fonts.families.values_mut() {
            family.push("cjk".into());
        }
    }
    ctx.set_fonts(fonts);
}

pub const CONTROL_RADIUS: u8 = 8;
pub const SURFACE_RADIUS: u8 = 12;

/// Resolved theme the daemon broadcasts via `ViewDiff::SetTheme` and the
/// `FrontendState` settings payload.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct ThemeSettings {
    #[serde(default)]
    pub palette: Palette,
}

/// Core palette channels used to derive egui visuals.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct Palette {
    #[serde(default)]
    pub bg: Option<String>,
    #[serde(default)]
    pub fg: Option<String>,
    #[serde(default)]
    pub muted: Option<String>,
    #[serde(default)]
    pub border: Option<String>,
    #[serde(default)]
    pub accent: Option<String>,
    #[serde(default)]
    pub warn: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub selection: Option<String>,
}

impl Palette {
    /// Resolved background, foreground, and accent colors (falling back to the
    /// egui dark defaults when a channel is absent or unparseable).
    pub fn resolved(&self) -> (egui::Color32, egui::Color32, egui::Color32) {
        let bg = self
            .bg
            .as_deref()
            .and_then(parse_color)
            .unwrap_or(egui::Color32::from_gray(27));
        let fg = self
            .fg
            .as_deref()
            .and_then(parse_color)
            .unwrap_or(egui::Color32::from_gray(230));
        let accent = self
            .accent
            .as_deref()
            .and_then(parse_color)
            .unwrap_or(egui::Color32::from_rgb(79, 156, 249));
        (bg, fg, accent)
    }
}

impl ThemeSettings {
    /// Build egui [`egui::Visuals`] from the resolved theme. Starts from the
    /// dark/light base chosen by the background luminance, then overlays the
    /// palette channels that are present. Absent channels keep the base value,
    /// so a partial theme stays legible. Surfaces deliberately stay close to
    /// the configured background: the transcript should read as a document,
    /// not a stack of black cards and gray controls.
    pub fn visuals(&self) -> egui::Visuals {
        let (bg, fg, accent) = self.palette.resolved();
        let dark = is_dark(bg);
        let mut visuals = if dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        let border = self
            .palette
            .border
            .as_deref()
            .and_then(parse_color)
            .unwrap_or_else(|| mix(bg, fg, 0.18));
        visuals.dark_mode = dark;
        visuals.panel_fill = bg;
        visuals.window_fill = mix(bg, fg, 0.045);
        visuals.faint_bg_color = mix(bg, fg, 0.035);
        visuals.extreme_bg_color = mix(bg, fg, 0.085);
        visuals.code_bg_color = mix(bg, fg, 0.075);
        visuals.override_text_color = Some(fg);
        visuals.weak_text_color = Some(
            self.palette
                .muted
                .as_deref()
                .and_then(parse_color)
                .unwrap_or_else(|| mix(bg, fg, 0.70)),
        );
        visuals.window_corner_radius = egui::CornerRadius::same(SURFACE_RADIUS);
        visuals.menu_corner_radius = egui::CornerRadius::same(CONTROL_RADIUS);
        visuals.hyperlink_color = accent;
        visuals.selection.bg_fill = self
            .palette
            .selection
            .as_deref()
            .and_then(parse_color)
            .unwrap_or_else(|| mix(bg, accent, 0.25));
        visuals.selection.stroke.color = fg;
        visuals.widgets.noninteractive.bg_fill = mix(bg, fg, 0.018);
        visuals.widgets.noninteractive.weak_bg_fill = mix(bg, fg, 0.012);
        visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, border);
        if let Some(color) = self.palette.error.as_deref().and_then(parse_color) {
            visuals.error_fg_color = color;
        }
        if let Some(color) = self.palette.warn.as_deref().and_then(parse_color) {
            visuals.warn_fg_color = color;
        }
        let inactive = mix(bg, fg, 0.045);
        let hovered = mix(bg, accent, 0.22);
        let active = mix(bg, accent, 0.34);
        visuals.widgets.inactive.weak_bg_fill = inactive;
        visuals.widgets.inactive.bg_fill = inactive;
        visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, border);
        visuals.widgets.hovered.weak_bg_fill = hovered;
        visuals.widgets.hovered.bg_fill = hovered;
        visuals.widgets.active.weak_bg_fill = active;
        visuals.widgets.active.bg_fill = active;
        for widget in [
            &mut visuals.widgets.noninteractive,
            &mut visuals.widgets.inactive,
            &mut visuals.widgets.hovered,
            &mut visuals.widgets.active,
            &mut visuals.widgets.open,
        ] {
            widget.corner_radius = egui::CornerRadius::same(CONTROL_RADIUS);
        }
        visuals
    }

    /// Terminal-like defaults: every text role is monospace, matching the TUI.
    pub fn style(&self) -> egui::Style {
        let mut style = egui::Style {
            visuals: self.visuals(),
            ..Default::default()
        };
        style.spacing.item_spacing = egui::vec2(8.0, 4.0);
        style.spacing.button_padding = egui::vec2(6.0, 2.0);
        style.spacing.interact_size = egui::vec2(24.0, 20.0);
        style.spacing.slider_width = 120.0;
        for (text_style, size) in [
            (egui::TextStyle::Body, 14.0),
            (egui::TextStyle::Button, 14.0),
            (egui::TextStyle::Small, 12.0),
            (egui::TextStyle::Heading, 15.0),
            (egui::TextStyle::Monospace, 14.0),
        ] {
            style
                .text_styles
                .insert(text_style, egui::FontId::monospace(size));
        }
        style
    }
}

/// Linear mix of two opaque colors, `t` in `[0, 1]`.
pub(crate) fn mix(a: egui::Color32, b: egui::Color32, t: f32) -> egui::Color32 {
    let t = t.clamp(0.0, 1.0);
    let lerp = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    egui::Color32::from_rgb(lerp(a.r(), b.r()), lerp(a.g(), b.g()), lerp(a.b(), b.b()))
}

/// Perceptual luminance test used to pick the egui dark/light base.
fn is_dark(color: egui::Color32) -> bool {
    let luma = 0.299 * color.r() as f32 + 0.587 * color.g() as f32 + 0.114 * color.b() as f32;
    luma < 128.0
}

/// Parse a color reference into an egui color. Accepts a `#rgb`/`#rrggbb`/
/// `#rrggbbaa` hex string or a named ANSI color (`red`, `lightgreen`, …).
/// Returns `None` for anything malformed so callers keep their previous color.
/// Palette-role names are resolved separately by [`resolve_color`].
pub fn parse_color(value: &str) -> Option<egui::Color32> {
    let value = value.trim();
    match value.strip_prefix('#') {
        Some(hex) => parse_hex(hex),
        None => named_color(value),
    }
}

/// Parse the body of a `#rgb`/`#rrggbb`/`#rrggbbaa` string.
fn parse_hex(hex: &str) -> Option<egui::Color32> {
    if !hex.is_ascii() {
        return None;
    }
    let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    match hex.len() {
        3 => {
            let expand = |c: char| u8::from_str_radix(&format!("{c}{c}"), 16).ok();
            let mut chars = hex.chars();
            Some(egui::Color32::from_rgb(
                expand(chars.next()?)?,
                expand(chars.next()?)?,
                expand(chars.next()?)?,
            ))
        }
        6 => Some(egui::Color32::from_rgb(
            channel(0)?,
            channel(2)?,
            channel(4)?,
        )),
        8 => Some(egui::Color32::from_rgba_unmultiplied(
            channel(0)?,
            channel(2)?,
            channel(4)?,
            channel(6)?,
        )),
        _ => None,
    }
}

/// Named ANSI color → RGB, mirroring the TUI's `color_to_rgb` mapping. Names
/// are matched case-insensitively and ignore `_`/`-`/spaces, so `DARK_GRAY`,
/// `dark-gray` and `darkgray` all resolve.
fn named_color(value: &str) -> Option<egui::Color32> {
    let key: String = value
        .chars()
        .filter(|c| !matches!(c, '_' | '-' | ' '))
        .collect::<String>()
        .to_ascii_lowercase();
    Some(match key.as_str() {
        "black" => egui::Color32::from_rgb(0x00, 0x00, 0x00),
        "red" => egui::Color32::from_rgb(0xcd, 0x31, 0x31),
        "green" => egui::Color32::from_rgb(0x0d, 0xbc, 0x79),
        "yellow" => egui::Color32::from_rgb(0xe5, 0xe5, 0x10),
        "blue" => egui::Color32::from_rgb(0x24, 0x72, 0xc8),
        "magenta" => egui::Color32::from_rgb(0xbc, 0x3f, 0xbc),
        "cyan" => egui::Color32::from_rgb(0x11, 0xa8, 0xcd),
        "gray" | "grey" => egui::Color32::from_rgb(0xc0, 0xc0, 0xc0),
        "darkgray" | "darkgrey" => egui::Color32::from_rgb(0x80, 0x80, 0x80),
        "lightred" => egui::Color32::from_rgb(0xf1, 0x4c, 0x4c),
        "lightgreen" => egui::Color32::from_rgb(0x23, 0xd1, 0x8b),
        "lightyellow" => egui::Color32::from_rgb(0xf5, 0xf5, 0x43),
        "lightblue" => egui::Color32::from_rgb(0x3b, 0x8e, 0xea),
        "lightmagenta" => egui::Color32::from_rgb(0xd6, 0x70, 0xd6),
        "lightcyan" => egui::Color32::from_rgb(0x29, 0xb8, 0xdb),
        "white" => egui::Color32::from_rgb(0xff, 0xff, 0xff),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_parses_resolved_theme_settings() {
        // Shape matches the daemon's serialized resolved ThemeSettings.
        let json = serde_json::json!({
            "name": "dark",
            "palette": {
                "bg": "#101014",
                "fg": "#e0e0e0",
                "accent": "#4f9cf9",
                "error": "#ff5555"
            },
            "user_msg": "#8be9fd",
            "thinking": "#6272a4",
            "unknown_future_field": true
        });
        let theme: ThemeSettings = serde_json::from_value(json).expect("parses");
        assert_eq!(theme.palette.bg.as_deref(), Some("#101014"));
        assert!(theme.palette.muted.is_none());
        let (bg, fg, accent) = theme.palette.resolved();
        assert_eq!(bg, egui::Color32::from_rgb(0x10, 0x10, 0x14));
        assert_eq!(fg, egui::Color32::from_rgb(0xe0, 0xe0, 0xe0));
        assert_eq!(accent, egui::Color32::from_rgb(0x4f, 0x9c, 0xf9));
    }

    #[test]
    fn visuals_follow_palette_and_luminance() {
        let dark: ThemeSettings = serde_json::from_value(serde_json::json!({
            "palette": { "bg": "#101014", "fg": "#e0e0e0", "accent": "#4f9cf9", "error": "#ff5555" }
        }))
        .unwrap();
        let visuals = dark.visuals();
        assert!(visuals.dark_mode);
        assert_eq!(
            visuals.panel_fill,
            egui::Color32::from_rgb(0x10, 0x10, 0x14)
        );
        assert_eq!(
            visuals.override_text_color,
            Some(egui::Color32::from_rgb(0xe0, 0xe0, 0xe0))
        );
        assert_eq!(
            visuals.hyperlink_color,
            egui::Color32::from_rgb(0x4f, 0x9c, 0xf9)
        );
        assert_eq!(
            visuals.error_fg_color,
            egui::Color32::from_rgb(0xff, 0x55, 0x55)
        );

        let light: ThemeSettings = serde_json::from_value(serde_json::json!({
            "palette": { "bg": "#f5f5f5", "fg": "#101014" }
        }))
        .unwrap();
        let visuals = light.visuals();
        assert!(!visuals.dark_mode);
        assert_eq!(
            visuals.panel_fill,
            egui::Color32::from_rgb(0xf5, 0xf5, 0xf5)
        );
    }

    #[test]
    fn style_is_monospace_like_the_terminal() {
        let style = ThemeSettings::default().style();
        for text_style in [
            egui::TextStyle::Body,
            egui::TextStyle::Button,
            egui::TextStyle::Heading,
            egui::TextStyle::Monospace,
        ] {
            assert_eq!(
                style.text_styles[&text_style].family,
                egui::FontFamily::Monospace
            );
        }
        assert_eq!(style.visuals.panel_fill, egui::Color32::from_gray(27));
    }

    #[test]
    fn bundled_fonts_render_all_families() {
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        ctx.set_style_of(ctx.theme(), ThemeSettings::default().style());
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.label("Inter body → ✓");
            ui.heading("Inter SemiBold heading");
            ui.monospace("JetBrains Mono: fn main() {}");
        });
        assert!(!output.shapes.is_empty());
        output.textures_delta.clear();
    }

    /// Bundled default spinner presets, read from the Lua the daemon ships so a
    /// new preset cannot silently introduce a glyph the desktop cannot draw.
    const SPINNER_PRESETS: &str = include_str!("../../core/defaults/lua/core/lib/ui/spinners.lua");

    /// Symbols the shared renderer draws with the monospace family: collapsed
    /// markers (`⋮`), live-pane headers (`✻`), job/process states (`⧗ ◑`), the
    /// block/box/arrow glyphs used by statuses and tool gutters, and the
    /// `kaomoji` accent glyphs.
    const CORE_UI_GLYPHS: &str = "⋮ ✻ ⧗ ◑ ◐ ✓ ✗ ✕ ⚠ │ ─ · — … › → ← ↑ ↓ ■ ● ○ ▸ ▲ ▶ ▼ ◀ ▏ ▎ ▍ ▌ ▋ ▊ ▉ ▁ ▃ ▅ ▇ ▂ ▄ ▆ ░ ▒ █ ╰ ╭ ╮ ╯ ├ ┤ ┼ ┏ ┓ ┗ ┛ ┃ ▰ ◕ ‿ ◠ ✿ ▽ ᴗ ~ ‾";

    /// Every frame/phrase literal in the bundled spinner presets.
    fn spinner_frames() -> Vec<String> {
        let mut frames = Vec::new();
        let mut entries = 0;
        for line in SPINNER_PRESETS.lines() {
            for key in ["frames = ", "phrases = "] {
                let Some((_, rest)) = line.split_once(key) else {
                    continue;
                };
                entries += 1;
                let mut chars = rest.chars();
                while let Some(ch) = chars.next() {
                    if ch != '"' {
                        continue;
                    }
                    let mut frame = String::new();
                    for ch in chars.by_ref() {
                        if ch == '"' {
                            break;
                        }
                        frame.push(ch);
                    }
                    frames.push(frame);
                }
            }
        }
        assert!(entries >= 8, "parsed only {entries} spinner entries");
        frames
    }

    /// The desktop paints the transcript, panes, and status bar on a character
    /// cell grid using the monospace family; a codepoint no face covers is
    /// drawn as egui's replacement box — the blank square users saw in place of
    /// the thinking spinner.
    #[test]
    fn monospace_family_covers_rendered_glyphs() {
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        // Fonts are only resolved after one pass.
        let mut output = ctx.run_ui(egui::RawInput::default(), |_ui| {});
        let mono = egui::FontId::monospace(14.0);
        // `has_glyph` is unusable here: it reports false for every char owned by
        // the face egui chose as its replacement-glyph face, and that face
        // (Adwaita Mono, which owns `◻`) covers exactly the symbols JetBrains
        // Mono lacks. Probe drawability by advance width instead: a char no
        // face covers resolves to a face without a glyph for it and measures 0.
        let missing = ctx.fonts_mut(|view| {
            let mut missing: Vec<String> = spinner_frames()
                .iter()
                .filter(|frame| frame.chars().any(|ch| view.glyph_width(&mono, ch) == 0.0))
                .cloned()
                .collect();
            missing.extend(
                CORE_UI_GLYPHS
                    .chars()
                    .filter(|ch| !ch.is_whitespace() && view.glyph_width(&mono, *ch) == 0.0)
                    .map(|ch| format!("U+{:04X} {ch}", ch as u32)),
            );
            assert!(view.glyph_width(&mono, 'A') > 0.0, "probe font missing");
            // Negative control: a codepoint none of the bundled or built-in
            // faces covers must measure zero, or the probe proves nothing.
            assert_eq!(
                view.glyph_width(&mono, '\u{10800}'),
                0.0,
                "probe undiscriminating"
            );
            missing
        });
        output.textures_delta.clear();
        assert!(missing.is_empty(), "undrawable glyphs: {missing:?}");
    }

    #[test]
    fn configured_border_and_surface_overrides_are_preserved() {
        let theme: ThemeSettings = serde_json::from_value(serde_json::json!({
            "palette": {
                "bg": "#101014",
                "fg": "#e0e0e0",
                "border": "#334455"
            }
        }))
        .unwrap();
        let visuals = theme.visuals();
        assert_eq!(
            visuals.widgets.noninteractive.bg_stroke.color,
            egui::Color32::from_rgb(0x33, 0x44, 0x55)
        );
        assert_eq!(
            visuals.panel_fill,
            egui::Color32::from_rgb(0x10, 0x10, 0x14)
        );
    }
    #[test]
    fn parse_color_handles_all_forms_and_rejects_junk() {
        assert_eq!(
            parse_color("#abc"),
            Some(egui::Color32::from_rgb(0xaa, 0xbb, 0xcc))
        );
        assert_eq!(
            parse_color("#112233"),
            Some(egui::Color32::from_rgb(0x11, 0x22, 0x33))
        );
        assert_eq!(
            parse_color("#11223344"),
            Some(egui::Color32::from_rgba_unmultiplied(
                0x11, 0x22, 0x33, 0x44
            ))
        );
        assert_eq!(parse_color("112233"), None);
        assert_eq!(parse_color("#12"), None);
        assert_eq!(parse_color("#gggggg"), None);
    }

    #[test]
    fn parse_color_accepts_named_ansi_colors_case_and_separator_insensitively() {
        assert_eq!(
            parse_color("red"),
            Some(egui::Color32::from_rgb(0xcd, 0x31, 0x31))
        );
        assert_eq!(
            parse_color("LightGreen"),
            Some(egui::Color32::from_rgb(0x23, 0xd1, 0x8b))
        );
        // Case- and separator-insensitive: `dark_gray`, `dark-gray`, `DARKGRAY`.
        let gray = Some(egui::Color32::from_rgb(0x80, 0x80, 0x80));
        assert_eq!(parse_color("dark_gray"), gray);
        assert_eq!(parse_color("dark-gray"), gray);
        assert_eq!(parse_color("DARKGRAY"), gray);
        // `grey` is an accepted alias; unknown names stay unresolved.
        assert_eq!(
            parse_color("grey"),
            Some(egui::Color32::from_rgb(0xc0, 0xc0, 0xc0))
        );
        assert_eq!(parse_color("chartreuse"), None);
    }
}

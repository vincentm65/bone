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

/// Embedded fonts keep the desktop consistent across machines; egui's original
/// fallback chain remains available for symbols and characters outside Latin.
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
    let mut semibold = proportional.clone();
    semibold.insert(0, "Inter SemiBold".into());
    fonts
        .families
        .insert(egui::FontFamily::Name("semibold".into()), semibold);
    fonts
        .families
        .get_mut(&egui::FontFamily::Monospace)
        .unwrap()
        .insert(0, "JetBrains Mono".into());
    ctx.set_fonts(fonts);
}

/// One shared width for the transcript and composer, including side padding.
pub const CHAT_WIDTH: f32 = 800.0;
pub const CHAT_PADDING: i8 = 20;
pub const CONTROL_HEIGHT: f32 = 34.0;
pub const CONTROL_RADIUS: u8 = 8;
pub const SURFACE_RADIUS: u8 = 12;

/// Resolved theme the daemon broadcasts via `ViewDiff::SetTheme` and the
/// `FrontendState` settings payload.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct ThemeSettings {
    #[serde(default)]
    pub palette: Palette,
    #[serde(default)]
    pub user_msg: Option<String>,
    #[serde(default)]
    pub user_msg_bg: Option<String>,
    #[serde(default)]
    pub system_msg: Option<String>,
    #[serde(default)]
    pub tool_call: Option<String>,
    #[serde(default)]
    pub tool_error: Option<String>,
    #[serde(default)]
    pub diff_removed: Option<String>,
    #[serde(default)]
    pub diff_added: Option<String>,
    #[serde(default)]
    pub thinking: Option<String>,
    #[serde(default)]
    pub markdown_marker: Option<String>,
    #[serde(default)]
    pub markdown_heading: Option<String>,
    #[serde(default)]
    pub markdown_link: Option<String>,
    #[serde(default)]
    pub markdown_inline_code: Option<String>,
    #[serde(default)]
    pub markdown_rule: Option<String>,
    #[serde(default)]
    pub markdown_table_border: Option<String>,
    #[serde(default)]
    pub markdown_table_header: Option<String>,
    #[serde(default)]
    pub heat_low: Option<String>,
    #[serde(default)]
    pub heat_high: Option<String>,
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
    pub subtle: Option<String>,
    #[serde(default)]
    pub border: Option<String>,
    #[serde(default)]
    pub accent: Option<String>,
    #[serde(default)]
    pub good: Option<String>,
    #[serde(default)]
    pub warn: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub selection: Option<String>,
}

/// Pre-resolved render roles. Defaults mirror the TUI's `derive_palette_roles`
/// so a theme that only sets a palette behaves like the terminal: user messages
/// use the foreground, reasoning/tool names the muted channel, tool errors and
/// approvals the error channel, etc. Each role may itself reference a palette
/// role, a named ANSI color, or a hex string.
#[derive(Debug, Clone)]
pub struct ThemeColors {
    pub user_msg: egui::Color32,
    pub user_msg_bg: egui::Color32,
    pub system_msg: egui::Color32,
    pub warn: egui::Color32,
    pub tool_call: egui::Color32,
    pub tool_error: egui::Color32,
    pub thinking: egui::Color32,
    pub markdown_marker: egui::Color32,
    pub markdown_heading: egui::Color32,
    pub markdown_link: egui::Color32,
    pub markdown_inline_code: egui::Color32,
    pub markdown_rule: egui::Color32,
    /// Border color for rendered Markdown tables.
    pub markdown_table_border: egui::Color32,
    /// Header text color for rendered Markdown tables.
    pub markdown_table_header: egui::Color32,
    /// Background fill for added (`+`) diff lines.
    pub diff_added: egui::Color32,
    /// Background fill for removed (`-`) diff lines.
    pub diff_removed: egui::Color32,
    /// Whether the background is dark; selects the syntect theme for code.
    pub syntax_dark: bool,
}

impl Default for ThemeColors {
    fn default() -> Self {
        ThemeSettings::default().colors()
    }
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

    /// Native defaults for readable conversation text and compact controls.
    /// Font roles remain explicit so code can opt into `Monospace` without
    /// making the surrounding conversation monospace.
    pub fn style(&self) -> egui::Style {
        let mut style = egui::Style {
            visuals: self.visuals(),
            ..Default::default()
        };
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(12.0, 7.0);
        style.spacing.interact_size = egui::vec2(CONTROL_HEIGHT, CONTROL_HEIGHT);
        style.spacing.slider_width = 120.0;
        style
            .text_styles
            .insert(egui::TextStyle::Body, egui::FontId::proportional(15.0));
        style
            .text_styles
            .insert(egui::TextStyle::Button, egui::FontId::proportional(14.0));
        style
            .text_styles
            .insert(egui::TextStyle::Small, egui::FontId::proportional(12.0));
        style.text_styles.insert(
            egui::TextStyle::Heading,
            egui::FontId::new(22.0, egui::FontFamily::Name("semibold".into())),
        );
        style
            .text_styles
            .insert(egui::TextStyle::Monospace, egui::FontId::monospace(14.0));
        style
    }

    /// Pre-resolve every render role into concrete colors. Absent roles inherit
    /// a palette-derived default (mirroring the TUI), so a partial theme stays
    /// legible; present roles are resolved through [`resolve_color`] so they may
    /// name a palette role, an ANSI color, or a hex string.
    pub fn colors(&self) -> ThemeColors {
        let palette = &self.palette;
        let (bg, fg, accent) = palette.resolved();
        let resolve =
            |value: &Option<String>| value.as_deref().and_then(|s| resolve_color(s, palette));
        let muted = resolve(&palette.muted).unwrap_or(fg);
        let subtle = resolve(&palette.subtle).unwrap_or(fg);
        let warn = resolve(&palette.warn).unwrap_or(accent);
        let error = resolve(&palette.error).unwrap_or(egui::Color32::from_rgb(235, 90, 90));
        let prompt_surface = mix(bg, fg, 0.075);
        ThemeColors {
            user_msg: resolve(&self.user_msg).unwrap_or(fg),
            user_msg_bg: resolve(&self.user_msg_bg).unwrap_or(prompt_surface),
            system_msg: resolve(&self.system_msg).unwrap_or(fg),
            warn,
            tool_call: resolve(&self.tool_call).unwrap_or(muted),
            tool_error: resolve(&self.tool_error).unwrap_or(error),
            thinking: resolve(&self.thinking).unwrap_or(accent),
            markdown_marker: resolve(&self.markdown_marker).unwrap_or(muted),
            markdown_heading: resolve(&self.markdown_heading).unwrap_or(fg),
            markdown_link: resolve(&self.markdown_link).unwrap_or(muted),
            markdown_inline_code: resolve(&self.markdown_inline_code).unwrap_or(muted),
            markdown_rule: resolve(&self.markdown_rule).unwrap_or(subtle),
            markdown_table_border: resolve(&self.markdown_table_border)
                .or_else(|| resolve(&palette.border))
                .unwrap_or(fg),
            markdown_table_header: resolve(&self.markdown_table_header).unwrap_or(accent),
            diff_added: resolve(&self.diff_added).unwrap_or(egui::Color32::from_rgb(0, 95, 0)),
            diff_removed: resolve(&self.diff_removed).unwrap_or(egui::Color32::from_rgb(135, 1, 1)),
            syntax_dark: is_dark(bg),
        }
    }

    /// Resolve themed heatmap colors. Empty cells remain subdued, and the
    /// default active gradient grows from a tinted surface to the accent.
    pub fn heat_colors(&self) -> (egui::Color32, egui::Color32, egui::Color32) {
        let palette = &self.palette;
        let (bg, fg, accent) = palette.resolved();
        let channel = |value: &Option<String>| value.as_deref().and_then(parse_color);
        let subtle = channel(&palette.subtle).unwrap_or_else(|| mix(bg, fg, 0.085));
        let good = channel(&palette.good).unwrap_or(accent);
        let high = self
            .heat_high
            .as_deref()
            .and_then(|value| resolve_color(value, palette))
            .unwrap_or(good);
        let low = self
            .heat_low
            .as_deref()
            .and_then(|value| resolve_color(value, palette))
            .unwrap_or_else(|| mix(bg, high, 0.3));
        (low, high, subtle)
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

/// Named palette role (`fg`, `accent`, `muted`, …) → egui color, mirroring the
/// TUI's `resolve_color_ref`. `muted`/`subtle`/`border` fall back to the
/// foreground; `good`/`warn`/`error`/`selection` return `None` when unset.
pub fn palette_role(value: &str, palette: &Palette) -> Option<egui::Color32> {
    let (bg, fg, accent) = palette.resolved();
    let channel = |name: &Option<String>| name.as_deref().and_then(parse_color);
    match value {
        "bg" => Some(bg),
        "fg" => Some(fg),
        "accent" => Some(accent),
        "muted" => channel(&palette.muted).or(Some(fg)),
        "subtle" => channel(&palette.subtle).or(Some(fg)),
        "border" => channel(&palette.border).or(Some(fg)),
        "good" => channel(&palette.good),
        "warn" => channel(&palette.warn),
        "error" => channel(&palette.error),
        "selection" => channel(&palette.selection),
        _ => None,
    }
}

/// Resolve a color reference: a palette role name, a named ANSI color, or a hex
/// string. Returns `None` for unknown references so callers keep their fallback.
pub fn resolve_color(value: &str, palette: &Palette) -> Option<egui::Color32> {
    palette_role(value, palette).or_else(|| parse_color(value))
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
        assert_eq!(theme.user_msg.as_deref(), Some("#8be9fd"));
        assert_eq!(theme.thinking.as_deref(), Some("#6272a4"));
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
    fn style_keeps_conversation_proportional_and_controls_compact() {
        let style = ThemeSettings::default().style();
        assert_eq!(
            style.text_styles[&egui::TextStyle::Body].family,
            egui::FontFamily::Proportional
        );
        assert_eq!(
            style.text_styles[&egui::TextStyle::Monospace].family,
            egui::FontFamily::Monospace
        );
        assert_eq!(style.spacing.item_spacing, egui::vec2(8.0, 6.0));
        assert_eq!(
            style.spacing.interact_size,
            egui::vec2(CONTROL_HEIGHT, CONTROL_HEIGHT)
        );
        assert_eq!(style.visuals.panel_fill, egui::Color32::from_gray(27));
        assert_eq!(
            style.text_styles[&egui::TextStyle::Heading].family,
            egui::FontFamily::Name("semibold".into())
        );
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

    #[test]
    fn resolve_color_prefers_palette_role_over_named_and_hex() {
        let palette: Palette = serde_json::from_value(serde_json::json!({
            "bg": "#101014",
            "fg": "#e0e0e0",
            "accent": "#4f9cf9",
            "muted": "#808080",
            "error": "#ff5555"
        }))
        .unwrap();
        // A palette role wins even though it is not a named/hex color.
        assert_eq!(
            resolve_color("accent", &palette),
            Some(egui::Color32::from_rgb(0x4f, 0x9c, 0xf9))
        );
        assert_eq!(
            resolve_color("muted", &palette),
            Some(egui::Color32::from_rgb(0x80, 0x80, 0x80))
        );
        // `muted` falls back to the foreground when the channel is unset.
        let bare: Palette = serde_json::from_value(serde_json::json!({ "fg": "#abcdef" })).unwrap();
        assert_eq!(
            resolve_color("muted", &bare),
            Some(egui::Color32::from_rgb(0xab, 0xcd, 0xef))
        );
        // `error` returns None when unset, so a hex/named string still resolves.
        assert_eq!(resolve_color("error", &bare), None);
        assert_eq!(
            resolve_color("red", &bare),
            Some(egui::Color32::from_rgb(0xcd, 0x31, 0x31))
        );
        assert_eq!(
            resolve_color("#123456", &bare),
            Some(egui::Color32::from_rgb(0x12, 0x34, 0x56))
        );
        assert_eq!(resolve_color("not-a-color", &bare), None);
    }

    #[test]
    fn colors_maps_roles_from_hex_named_and_palette_references() {
        let theme: ThemeSettings = serde_json::from_value(serde_json::json!({
            "palette": {
                "bg": "#101014",
                "fg": "#e0e0e0",
                "accent": "#4f9cf9",
                "muted": "#808080",
                "error": "#ff5555"
            },
            // hex, a named ANSI color, and a palette-role reference respectively.
            "tool_call": "accent",
            "markdown_heading": "LightGreen",
            "thinking": "#123456"
        }))
        .unwrap();
        let colors = theme.colors();
        assert_eq!(colors.tool_call, egui::Color32::from_rgb(0x4f, 0x9c, 0xf9));
        assert_eq!(
            colors.markdown_heading,
            egui::Color32::from_rgb(0x23, 0xd1, 0x8b)
        );
        assert_eq!(colors.thinking, egui::Color32::from_rgb(0x12, 0x34, 0x56));
        // Palette-derived defaults still apply to unset roles.
        assert_eq!(colors.user_msg, egui::Color32::from_rgb(0xe0, 0xe0, 0xe0));
        assert_eq!(
            colors.markdown_marker,
            egui::Color32::from_rgb(0x80, 0x80, 0x80)
        );
        assert_eq!(colors.tool_error, egui::Color32::from_rgb(0xff, 0x55, 0x55));
    }
}

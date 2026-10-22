//! Drawing the GUI with egui: the Lua-laid-out screen (`screen.rs`),
//! questions as they come, and Lua styles as egui text formats. Lua-produced
//! lines are monospace, so a column here is a column in the TUI. Around that
//! grid the window has its own chrome: theme, margin, menus and dialogs.

use bone_ui::options::DynamicValue;
use bone_ui::style::{Color, Modifier, Style};
use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{self, Color32, FontId, Key, Stroke};
use serde_json::Value;
use unicode_width::UnicodeWidthChar;

use crate::{Gui, Row};

impl Gui {
    /// Draw one frame into `ui` (the whole window).
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let _ = self.repaint.set(ui.ctx().clone());
        self.pump();
        self.switch_library();
        // New fonts take effect on a fresh pass: lay nothing out with the
        // old cell size.
        if self.apply_fonts(ui.ctx()) {
            ui.ctx().request_discard("fonts changed");
            return;
        }
        self.apply_visuals(ui.ctx());
        self.route_keys(ui.ctx());
        self.dropped_files(ui.ctx());
        self.window_ui(ui.ctx());
        self.menu_ui(ui);
        let canvas = self.canvas();
        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .inner_margin(egui::Margin::symmetric(16, 10))
                    .fill(canvas),
            )
            .show(ui, |ui| self.screen_ui(ui));
        self.question_ui(ui.ctx());
        self.preview_ui(ui.ctx());
    }

    /// The window's background: the Normal group's, or a dark canvas when
    /// a scheme leaves it unset (the bare library).
    pub(crate) fn canvas(&self) -> Color32 {
        self.lua
            .theme
            .hl("Normal")
            .bg
            .and_then(color)
            .unwrap_or(Color32::from_rgb(0x16, 0x16, 0x16))
    }

    /// Menus, dialogs and scrollbars follow the Lua colors. Once per
    /// colorscheme: rewriting the style every frame asks for another.
    fn apply_visuals(&mut self, ctx: &egui::Context) {
        if self.visuals_rev == Some(self.lua.opts_rev) {
            return;
        }
        self.visuals_rev = Some(self.lua.opts_rev);
        let canvas = self.canvas();
        let raised = lifted(canvas);
        let fg = self
            .lua
            .theme
            .hl("Normal")
            .fg
            .and_then(color)
            .unwrap_or(Color32::from_rgb(0xee, 0xee, 0xee));
        let accent = self
            .lua
            .theme
            .hl("Accent")
            .fg
            .and_then(color)
            .unwrap_or(Color32::from_rgb(0xc8, 0xdc, 0xf5));
        let rule = self
            .lua
            .theme
            .hl("WinSeparator")
            .fg
            .and_then(color)
            .unwrap_or(Color32::from_rgb(0x2c, 0x2c, 0x2c));
        let mut vis = egui::Visuals::dark();
        vis.panel_fill = canvas;
        vis.window_fill = raised;
        vis.extreme_bg_color = canvas;
        vis.faint_bg_color = raised;
        vis.override_text_color = Some(fg);
        vis.hyperlink_color = accent;
        vis.selection.bg_fill = Color32::from_rgb(0x2a, 0x3f, 0x55);
        vis.selection.stroke.color = accent;
        vis.window_corner_radius = egui::CornerRadius::same(8);
        vis.menu_corner_radius = egui::CornerRadius::same(6);
        vis.window_stroke = Stroke::new(1.0, rule);
        vis.popup_shadow = egui::epaint::Shadow {
            offset: [0, 8],
            blur: 18,
            spread: 0,
            color: Color32::from_black_alpha(90),
        };
        let radius = egui::CornerRadius::same(6);
        for widget in [
            &mut vis.widgets.noninteractive,
            &mut vis.widgets.inactive,
            &mut vis.widgets.hovered,
            &mut vis.widgets.active,
            &mut vis.widgets.open,
        ] {
            widget.corner_radius = radius;
        }
        vis.widgets.inactive.bg_fill = raised;
        vis.widgets.hovered.bg_fill = lifted(raised);
        vis.widgets.active.bg_fill = lifted(lifted(raised));
        ctx.set_visuals(vis);
        ctx.style_mut_of(ctx.theme(), |style| {
            // Over the transcript, not in place of its last column.
            style.spacing.scroll.floating = true;
            style.spacing.scroll.bar_width = 8.0;
            style.spacing.scroll.floating_width = 6.0;
        });
    }

    /// The oldest open question, as a modal.
    fn question_ui(&mut self, ctx: &egui::Context) {
        let Some((&ask_id, ask)) = self.asks.iter().next() else {
            return;
        };
        let text = Gui::question_text(&ask.question);
        let options = Gui::question_options(&ask.question);
        let mut answer: Option<Value> = None;
        egui::Modal::new(egui::Id::new(("ask", ask_id))).show(ctx, |ui| {
            ui.set_max_width(440.0);
            ui.label(egui::RichText::new(text).size(15.0));
            ui.add_space(8.0);
            if options.is_empty() {
                let edit = ui.text_edit_singleline(&mut self.answer_draft);
                let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                if ui.button("Answer").clicked() || enter {
                    answer = Some(Value::String(std::mem::take(&mut self.answer_draft)));
                }
            } else {
                ui.horizontal(|ui| {
                    for o in &options {
                        if ui.button(o).clicked() {
                            answer = Some(Value::String(o.clone()));
                        }
                    }
                });
            }
        });
        if let Some(a) = answer {
            self.answer(ask_id, a);
        }
    }
}

/// A chat row as text in its styles, one font for all of it, every
/// character as many cells wide as it is in a terminal (`col` points a
/// cell): a wide character, or one from a fallback font, is padded or
/// tucked in after it so the columns after it line up.
pub(crate) fn row_job(ui: &egui::Ui, row: &Row, font: &FontId, col: f32) -> LayoutJob {
    let visuals = ui.visuals();
    let mut job = LayoutJob::default();
    ui.ctx().fonts_mut(|fonts| {
        let mut lead = 0.0;
        for (text, style) in row {
            let format = format(style, font, visuals);
            let mut start = 0;
            // A character with the zero-width ones after it (combining
            // marks) is one cluster; the monospace font's own are one cell.
            let mut chars = text.char_indices().peekable();
            while let Some((i, c)) = chars.next() {
                let mut end = i + c.len_utf8();
                while let Some((j, m)) =
                    chars.next_if(|&(_, m)| !m.is_ascii() && m.width() == Some(0))
                {
                    end = j + m.len_utf8();
                }
                let cluster = &text[i..end];
                if cluster.is_ascii() {
                    continue;
                }
                let gap = c.width().unwrap_or(0) as f32 * col - advance(fonts, font, cluster);
                if gap.abs() > 0.01 {
                    job.append(&text[start..end], lead, format.clone());
                    (start, lead) = (end, gap);
                }
            }
            if start < text.len() {
                job.append(&text[start..], lead, format);
                lead = 0.0;
            }
        }
    });
    // An empty row is still a line tall.
    if job.text.is_empty() {
        job.append(
            " ",
            0.0,
            TextFormat::simple(font.clone(), Color32::TRANSPARENT),
        );
    }
    job
}

/// How far `cluster` moves the text along, in whichever fonts draw it
/// (egui keeps the layouts of recent frames).
fn advance(fonts: &mut egui::epaint::text::FontsView<'_>, font: &FontId, cluster: &str) -> f32 {
    let galley = fonts.layout_no_wrap(cluster.to_owned(), font.clone(), Color32::WHITE);
    let glyphs = galley.rows.iter().flat_map(|r| r.glyphs.iter());
    glyphs.map(|g| g.advance_width).sum()
}

fn format(style: &Style, font: &FontId, visuals: &egui::Visuals) -> TextFormat {
    let m = style.add_modifier;
    let mut fg = style.fg.and_then(color).unwrap_or(visuals.text_color());
    let mut bg = style.bg.and_then(color).unwrap_or(Color32::TRANSPARENT);
    if m.contains(Modifier::REVERSED) {
        let back = if bg == Color32::TRANSPARENT {
            visuals.panel_fill
        } else {
            bg
        };
        (fg, bg) = (back, fg);
    }
    if m.contains(Modifier::DIM) {
        fg = fg.gamma_multiply(0.6);
    }
    // egui's bundled fonts have no bold face: bold is a brighter color.
    if m.contains(Modifier::BOLD) {
        fg = fg.lerp_to_gamma(visuals.strong_text_color(), 0.5);
    }
    let line = |on: bool| {
        if on {
            Stroke::new(1.0, fg)
        } else {
            Stroke::NONE
        }
    };
    TextFormat {
        font_id: font.clone(),
        color: fg,
        background: bg,
        italics: m.contains(Modifier::ITALIC),
        underline: line(m.contains(Modifier::UNDERLINED)),
        strikethrough: line(m.contains(Modifier::CROSSED_OUT)),
        ..Default::default()
    }
}

/// A step lighter, for a bar or a card on `c`.
fn lifted(c: Color32) -> Color32 {
    Color32::from_rgb(
        c.r().saturating_add(14),
        c.g().saturating_add(14),
        c.b().saturating_add(14),
    )
}

/// The 16 ANSI colors, as a dark terminal shows them.
const ANSI: [(u8, u8, u8); 16] = [
    (0x1d, 0x1f, 0x21),
    (0xcc, 0x66, 0x66),
    (0xb5, 0xbd, 0x68),
    (0xf0, 0xc6, 0x74),
    (0x81, 0xa2, 0xbe),
    (0xb2, 0x94, 0xbb),
    (0x8a, 0xbe, 0xb7),
    (0xc5, 0xc8, 0xc6),
    (0x66, 0x66, 0x66),
    (0xd5, 0x4e, 0x53),
    (0xb9, 0xca, 0x4a),
    (0xe7, 0xc5, 0x47),
    (0x7a, 0xa6, 0xda),
    (0xc3, 0x97, 0xd8),
    (0x70, 0xc0, 0xb1),
    (0xea, 0xea, 0xea),
];

/// `None` for the default color.
pub(crate) fn color(c: Color) -> Option<Color32> {
    let ansi = |i: usize| {
        let (r, g, b) = ANSI[i];
        Some(Color32::from_rgb(r, g, b))
    };
    match c {
        Color::Reset => None,
        Color::Black => ansi(0),
        Color::Red => ansi(1),
        Color::Green => ansi(2),
        Color::Yellow => ansi(3),
        Color::Blue => ansi(4),
        Color::Magenta => ansi(5),
        Color::Cyan => ansi(6),
        Color::Gray => ansi(7),
        Color::DarkGray => ansi(8),
        Color::LightRed => ansi(9),
        Color::LightGreen => ansi(10),
        Color::LightYellow => ansi(11),
        Color::LightBlue => ansi(12),
        Color::LightMagenta => ansi(13),
        Color::LightCyan => ansi(14),
        Color::White => ansi(15),
        Color::Rgb(r, g, b) => Some(Color32::from_rgb(r, g, b)),
        Color::Indexed(n) => Some(indexed(n)),
    }
}

/// The xterm 256-color palette.
fn indexed(n: u8) -> Color32 {
    match n {
        0..=15 => {
            let (r, g, b) = ANSI[n as usize];
            Color32::from_rgb(r, g, b)
        }
        16..=231 => {
            let n = n - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Color32::from_rgb(level(n / 36), level(n / 6 % 6), level(n % 6))
        }
        232..=255 => {
            let v = 8 + (n - 232) * 10;
            Color32::from_rgb(v, v, v)
        }
    }
}

impl Gui {
    /// `bone.o.font` and `bone.o.font_size`, applied when they change;
    /// true when they did.
    fn apply_fonts(&mut self, ctx: &egui::Context) -> bool {
        let text = |name: &str| match self.lua.dynamic_options.get(name).map(|o| &o.value) {
            Some(DynamicValue::String(s)) => s.clone(),
            _ => String::new(),
        };
        let size = match self.lua.dynamic_options.get("font_size").map(|o| &o.value) {
            Some(DynamicValue::Number(n)) => *n as f32,
            Some(DynamicValue::Integer(n)) => *n as f32,
            _ => 14.0,
        };
        let sans = ["font_sans", "font_sans_bold", "font_sans_italic"].map(text);
        let want = (text("font"), size.clamp(6.0, 72.0), sans);
        if self.lua.fonts.as_ref() == Some(&want) {
            return false;
        }
        let mut fonts = egui::FontDefinitions::default();
        let mut add = |name: &str, path: &str| -> bool {
            match std::fs::read(path) {
                Ok(bytes) => {
                    let data = std::sync::Arc::new(egui::FontData::from_owned(bytes));
                    fonts.font_data.insert(name.into(), data);
                    true
                }
                Err(_) => false,
            }
        };
        let mut errors = Vec::new();
        let user = !want.0.is_empty() && add("user", &want.0);
        if !want.0.is_empty() && !user {
            errors.push(want.0.clone());
        }
        // The proportional faces: the options' files, else the system's.
        let mut faces = [false; 3];
        for (i, (opt, found)) in want.2.iter().zip(system_sans()).enumerate() {
            let path = if opt.is_empty() {
                found
            } else {
                Some(opt.clone())
            };
            if let Some(path) = path {
                faces[i] = add(["sans", "sans-bold", "sans-italic"][i], &path);
                if !faces[i] && !opt.is_empty() {
                    errors.push(path);
                }
            }
        }
        if user && let Some(mono) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
            mono.insert(0, "user".into());
        }
        let fallback = fonts.families[&egui::FontFamily::Proportional].clone();
        if faces[0]
            && let Some(p) = fonts.families.get_mut(&egui::FontFamily::Proportional)
        {
            p.insert(0, "sans".into());
        }
        let sans = fonts.families[&egui::FontFamily::Proportional].clone();
        for (i, face) in [(1, "sans-bold"), (2, "sans-italic")] {
            let mut family = sans.clone();
            if faces[i] {
                family.insert(0, face.into());
            }
            family.extend(fallback.iter().cloned());
            fonts
                .families
                .insert(egui::FontFamily::Name(face.into()), family);
        }
        for path in errors {
            self.error(format!("cannot read font {path}"));
        }
        self.lua.faces = (faces[1], faces[2]);
        ctx.set_fonts(fonts);
        ctx.all_styles_mut(|style| {
            if let Some(f) = style.text_styles.get_mut(&egui::TextStyle::Monospace) {
                f.size = want.1;
            }
            // A blinking cursor is a frame every half second, and each runs
            // the Lua parts of the screen: the cursor stays still.
            style.visuals.text_cursor.blink = false;
        });
        self.lua.fonts = Some(want);
        // Cached rows were wrapped for the old cell size.
        self.lua.views_rev += 1;
        true
    }
}

/// The system's sans-serif regular, bold and italic font files, where the
/// usual ones are (Noto, DejaVu, Liberation; Arial on macOS, Segoe UI on
/// Windows).
fn system_sans() -> [Option<String>; 3] {
    let sets: &[[&str; 3]] = &[
        [
            "/usr/share/fonts/noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/noto/NotoSans-Bold.ttf",
            "/usr/share/fonts/noto/NotoSans-Italic.ttf",
        ],
        [
            "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/truetype/noto/NotoSans-Bold.ttf",
            "/usr/share/fonts/truetype/noto/NotoSans-Italic.ttf",
        ],
        [
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
            "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf",
            "/usr/share/fonts/TTF/DejaVuSans-Oblique.ttf",
        ],
        [
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans-Oblique.ttf",
        ],
        [
            "/usr/share/fonts/liberation/LiberationSans-Regular.ttf",
            "/usr/share/fonts/liberation/LiberationSans-Bold.ttf",
            "/usr/share/fonts/liberation/LiberationSans-Italic.ttf",
        ],
        [
            "/System/Library/Fonts/Supplemental/Arial.ttf",
            "/System/Library/Fonts/Supplemental/Arial Bold.ttf",
            "/System/Library/Fonts/Supplemental/Arial Italic.ttf",
        ],
        [
            "C:\\Windows\\Fonts\\segoeui.ttf",
            "C:\\Windows\\Fonts\\segoeuib.ttf",
            "C:\\Windows\\Fonts\\segoeuii.ttf",
        ],
    ];
    let set = sets.iter().find(|s| std::path::Path::new(s[0]).exists());
    set.map_or([None, None, None], |s| {
        s.map(|p| std::path::Path::new(p).exists().then(|| p.to_owned()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Where each glyph of `text` starts, in cells.
    fn columns(text: &str) -> Vec<(char, f32)> {
        let ctx = egui::Context::default();
        let mut out = Vec::new();
        let mut frame = ctx.run_ui(egui::RawInput::default(), |ui| {
            let font = FontId::monospace(14.0);
            let col = ui.ctx().fonts_mut(|f| f.glyph_width(&font, 'M'));
            let row = vec![(text.to_owned(), Style::default())];
            let galley = ui.painter().layout_job(row_job(ui, &row, &font, col));
            let glyphs = galley.rows.iter().flat_map(|r| r.glyphs.iter());
            out = glyphs.map(|g| (g.chr, g.pos.x / col)).collect();
        });
        frame.textures_delta.clear();
        out
    }

    /// `text`'s characters start at these cells, give or take the pixel
    /// rounding egui does.
    fn assert_columns(text: &str, want: &[f32]) {
        let got = columns(text);
        let near =
            got.len() == want.len() && got.iter().zip(want).all(|((_, g), w)| (g - w).abs() < 0.15);
        assert!(near, "{text:?}: {got:?}, not {want:?}");
    }

    #[test]
    fn wide_and_fallback_characters_keep_the_columns() {
        // An emoji is two cells; box drawing comes from a fallback font.
        assert_columns("😀ab", &[0.0, 2.0, 3.0]);
        assert_columns("a─┼b", &[0.0, 1.0, 2.0, 3.0]);
        assert_columns("ab😀😀c", &[0.0, 1.0, 2.0, 4.0, 6.0]);
        assert_columns("e\u{301}x", &[0.0, 1.0, 1.0]);
    }
}

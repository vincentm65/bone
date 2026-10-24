//! One frame of the GUI with egui: the events and keys taken first, the
//! colorscheme and fonts applied to what egui draws itself, then the
//! window, which is Lua's node tree (`nodeview`), or a line saying why
//! there is none.

use bone_ui::options::DynamicValue;
use bone_ui::style::Color;
use eframe::egui::{self, Color32, Stroke};

use crate::Gui;

impl Gui {
    /// Draw one frame into `ui` (the whole window).
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let _ = self.repaint.set(ui.ctx().clone());
        self.note_focus(ui.ctx());
        self.pump();
        self.switch_library();
        // New fonts take effect on a fresh pass: measure nothing with the
        // old ones.
        if self.apply_fonts(ui.ctx()) {
            ui.ctx().request_discard("fonts changed");
            return;
        }
        self.apply_visuals(ui.ctx());
        self.route_keys(ui.ctx());
        self.dropped_files(ui.ctx());
        self.window_ui(ui.ctx());
        let canvas = self.canvas();
        // The node view (`bone.gui.view`) is the whole window.
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(canvas))
            .show(ui, |ui| {
                if !self.view_ui(ui) {
                    self.no_view_ui(ui);
                }
            });
        self.sweep_previews();
    }

    /// The window's background: the Normal group's, or a dark canvas when
    /// a scheme leaves it unset (the bare library), with the `opacity`
    /// option's alpha in a see-through window.
    pub(crate) fn canvas(&self) -> Color32 {
        let c = self
            .group_color("Normal", true)
            .unwrap_or(Color32::from_rgb(0x16, 0x16, 0x16));
        if !self.transparent {
            return c;
        }
        let a = (self.opacity() * 255.0).round() as u8;
        Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
    }

    /// The `opacity` option: how much of the window's background covers
    /// what is behind it, 0 to 1.
    pub(crate) fn opacity(&self) -> f32 {
        self.number_option("opacity", 1.0).clamp(0.0, 1.0)
    }

    /// Group `name`'s background (or foreground), if the colorscheme sets it.
    pub(crate) fn group_color(&self, name: &str, bg: bool) -> Option<Color32> {
        let s = self.lua.theme.hl(name);
        if bg { s.bg } else { s.fg }.and_then(color)
    }

    /// A number option of the GUI (`bone.o.define`), or `default`.
    pub(crate) fn number_option(&self, name: &str, default: f32) -> f32 {
        match self.lua.dynamic_options.get(name).map(|o| &o.value) {
            Some(DynamicValue::Number(n)) => *n as f32,
            Some(DynamicValue::Integer(n)) => *n as f32,
            _ => default,
        }
    }

    /// What egui draws itself (the prompt's cursor and selection, the
    /// selected text, widget corners) follows the colorscheme's groups and
    /// the GUI's options, with a fallback for a scheme that sets none.
    /// Once per colorscheme: rewriting the style every frame asks for
    /// another.
    ///
    /// Groups: `Normal` (window and text), `Panel` (raised surfaces),
    /// `Accent` (links), `TextSelection` (selected text: bg, and fg for its
    /// outline), `Cursor` (the text cursor), `WinSeparator` (outlines).
    /// Options: `widget_radius`, `window_radius`.
    fn apply_visuals(&mut self, ctx: &egui::Context) {
        if self.visuals_rev == Some(self.lua.opts_rev) {
            return;
        }
        self.visuals_rev = Some(self.lua.opts_rev);
        let canvas = self.canvas();
        let raised = self
            .group_color("Panel", true)
            .unwrap_or_else(|| lifted(canvas));
        let fg = self
            .group_color("Normal", false)
            .unwrap_or(Color32::from_rgb(0xee, 0xee, 0xee));
        let accent = self
            .group_color("Accent", false)
            .unwrap_or(Color32::from_rgb(0xc8, 0xdc, 0xf5));
        let rule = self
            .group_color("WinSeparator", false)
            .unwrap_or(Color32::from_rgb(0x2c, 0x2c, 0x2c));
        let mut vis = egui::Visuals::dark();
        vis.panel_fill = canvas;
        vis.window_fill = raised;
        vis.extreme_bg_color = canvas;
        vis.faint_bg_color = raised;
        vis.override_text_color = Some(fg);
        vis.hyperlink_color = accent;
        vis.selection.bg_fill = self
            .group_color("TextSelection", true)
            .unwrap_or(Color32::from_rgb(0x2a, 0x3f, 0x55));
        vis.selection.stroke.color = self.group_color("TextSelection", false).unwrap_or(accent);
        vis.text_cursor.stroke.color = self.group_color("Cursor", false).unwrap_or(fg);
        vis.window_corner_radius = egui::CornerRadius::same(
            self.number_option("window_radius", 8.0).clamp(0.0, 40.0) as u8,
        );
        vis.window_stroke = Stroke::new(1.0, rule);
        vis.popup_shadow = egui::epaint::Shadow {
            offset: [0, 8],
            blur: 18,
            spread: 0,
            color: Color32::from_black_alpha(90),
        };
        let radius = egui::CornerRadius::same(
            self.number_option("widget_radius", 6.0).clamp(0.0, 40.0) as u8,
        );
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
}

/// A step lighter, for a bar or a card on `c`.
pub(crate) fn lifted(c: Color32) -> Color32 {
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
    use Color::*;
    let ansi = match c {
        Reset => return None,
        Rgb(r, g, b) => return Some(Color32::from_rgb(r, g, b)),
        Indexed(n) => return Some(indexed(n)),
        Black => 0,
        Red => 1,
        Green => 2,
        Yellow => 3,
        Blue => 4,
        Magenta => 5,
        Cyan => 6,
        Gray => 7,
        DarkGray => 8,
        LightRed => 9,
        LightGreen => 10,
        LightYellow => 11,
        LightBlue => 12,
        LightMagenta => 13,
        LightCyan => 14,
        White => 15,
    };
    Some(indexed(ansi))
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
        let want = (
            text("font"),
            size.clamp(6.0, 72.0),
            sans,
            self.view.bound.extra_fonts.clone(),
        );
        if self.view.fonts.as_ref() == Some(&want) {
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
        // Symbols the text faces lack (box drawing, arrows, braille): every
        // family ends with whichever of these the system has.
        for (i, path) in system_symbols().iter().enumerate() {
            let name = format!("symbols{i}");
            if add(&name, path) {
                for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                    if let Some(f) = fonts.families.get_mut(&family) {
                        f.push(name.clone());
                    }
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
        // Families Lua registered: each is its own name, then the fallbacks
        // (so symbols the font lacks still draw).
        for (name, path) in &want.3 {
            let key = format!("lua:{name}");
            if add(&key, path) {
                let mut family = vec![key];
                family.extend(fallback.iter().cloned());
                fonts
                    .families
                    .insert(egui::FontFamily::Name(name.as_str().into()), family);
            } else {
                errors.push(path.clone());
            }
        }
        for path in errors {
            self.error(format!("cannot read font {path}"));
        }
        self.view.faces = (faces[1], faces[2]);
        ctx.set_fonts(fonts);
        ctx.all_styles_mut(|style| {
            if let Some(f) = style.text_styles.get_mut(&egui::TextStyle::Monospace) {
                f.size = want.1;
            }
            // A blinking cursor is a frame every half second, and each runs
            // the Lua parts of the screen: the cursor stays still.
            style.visuals.text_cursor.blink = false;
        });
        self.view.fonts = Some(want);
        // The monospace cell is derived from these fonts.  Force the next
        // frame to measure it again before building Lua's node tree; otherwise
        // a rebuilt view receives the old `ctx.cell` forever.
        self.view.cell = None;
        self.view.screen_size = (0, 0);
        self.view.screen_cols = 0;
        // Chat items made with the old fonts are made again.
        self.lua.views_rev += 1;
        true
    }
}

/// Fonts with the symbol blocks the sans faces lack, where the usual ones are.
fn system_symbols() -> Vec<String> {
    [
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/noto/NotoSansSymbols2-Regular.ttf",
        "/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf",
        "/System/Library/Fonts/Apple Symbols.ttf",
        "C:\\Windows\\Fonts\\seguisym.ttf",
    ]
    .into_iter()
    .filter(|p| std::path::Path::new(p).exists())
    .map(str::to_owned)
    .collect()
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

//! One frame of the GUI: the window's input routed (Lua's `mouse`
//! handlers, the keys), the fonts applied when they change, then the
//! window, which is Lua's node tree (`nodeview`), or a line saying why
//! there is none.

use bone_ui::options::DynamicValue;
use bone_ui::style::Color;

use crate::Gui;
use crate::backend::{Backend, FontSet, FrameInput, FrameOutput, Shape};
use crate::node::{Corners, Rgba};
use crate::nodeview::Fx;

impl Gui {
    /// Run one frame on `input`: what the window should draw and do.
    pub fn frame(&mut self, mut input: FrameInput, backend: &mut dyn Backend) -> FrameOutput {
        let mut out = FrameOutput::default();
        self.note_focus(input.focused);
        self.pump();
        self.switch_library();
        self.apply_fonts(backend);
        self.route_pointer(&mut input.events);
        self.view.pointer.begin(&input);
        self.dropped_files(&input);
        self.window_ui(&input, &mut out);
        let shaper = backend.shaper();
        let mut fx = Fx {
            input: &input,
            shaper,
            out: &mut out,
            typed: Vec::new(),
            opacity: 1.0,
        };
        self.route_keys(&mut fx);
        // The node view (`bone.gui.view`) is the whole window, on the canvas.
        let screen = input.screen;
        fx.paint(
            Shape::Rect {
                rect: screen,
                corners: Corners::default(),
                fill: self.canvas(),
            },
            screen,
        );
        if !self.view_ui(&mut fx) {
            self.no_view_ui(&mut fx);
        }
        self.sweep_previews();
        if let Some(t) = self.lua.copied.take() {
            out.copy = Some(t);
        }
        out.title = Some((self.window_title.clone()).unwrap_or_else(|| self.window_name()));
        out.close = self.closed;
        out
    }

    /// The window's background: the Normal group's, or a dark canvas when
    /// a scheme leaves it unset (the bare library), with the `opacity`
    /// option's alpha in a see-through window.
    pub(crate) fn canvas(&self) -> Rgba {
        let c = self
            .group_color("Normal", true)
            .unwrap_or(Rgba(0x16, 0x16, 0x16, 0xff));
        if !self.transparent {
            return c;
        }
        let a = (self.opacity() * 255.0).round() as u8;
        Rgba(c.0, c.1, c.2, a)
    }

    /// The window's name for the system (the taskbar, the window list):
    /// the chat's title, or "New chat" before it has one.
    fn window_name(&self) -> String {
        let session = self.chats[self.current].session.as_ref();
        (session.and_then(|s| s.title.clone()))
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| "New chat".into())
    }

    /// A true-or-false option of the GUI (`bone.o.define`), or `default`.
    pub(crate) fn bool_option(&self, name: &str, default: bool) -> bool {
        match self.lua.dynamic_options.get(name).map(|o| &o.value) {
            Some(DynamicValue::Boolean(b)) => *b,
            _ => default,
        }
    }

    /// The `opacity` option: how much of the window's background covers
    /// what is behind it, 0 to 1.
    pub(crate) fn opacity(&self) -> f32 {
        self.number_option("opacity", 1.0).clamp(0.0, 1.0)
    }

    /// Group `name`'s background (or foreground), if the colorscheme sets it.
    pub(crate) fn group_color(&self, name: &str, bg: bool) -> Option<Rgba> {
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

    /// `bone.o.font_size`: the monospace font's size.
    pub(crate) fn mono_size(&self) -> f32 {
        self.number_option("font_size", 14.0).clamp(6.0, 72.0)
    }
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
pub(crate) fn color(c: Color) -> Option<Rgba> {
    use Color::*;
    let ansi = match c {
        Reset => return None,
        Rgb(r, g, b) => return Some(Rgba(r, g, b, 0xff)),
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
fn indexed(n: u8) -> Rgba {
    match n {
        0..=15 => {
            let (r, g, b) = ANSI[n as usize];
            Rgba(r, g, b, 0xff)
        }
        16..=231 => {
            let n = n - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Rgba(level(n / 36), level(n / 6 % 6), level(n % 6), 0xff)
        }
        232..=255 => {
            let v = 8 + (n - 232) * 10;
            Rgba(v, v, v, 0xff)
        }
    }
}

impl Gui {
    /// `bone.o.font`, `font_size`, the sans faces and Lua's families,
    /// applied when they change.
    fn apply_fonts(&mut self, backend: &mut dyn Backend) {
        let text = |name: &str| match self.lua.dynamic_options.get(name).map(|o| &o.value) {
            Some(DynamicValue::String(s)) => s.clone(),
            _ => String::new(),
        };
        let size = self.mono_size();
        let sans = ["font_sans", "font_sans_bold", "font_sans_italic"].map(text);
        let want = (
            text("font"),
            size,
            sans,
            self.view.bound.extra_fonts.clone(),
        );
        if self.view.fonts.as_ref() == Some(&want) {
            return;
        }
        // The proportional faces: the options' files, else the built-in ones.
        let mut faces: [Option<String>; 3] = [None, None, None];
        for (i, opt) in want.2.iter().enumerate() {
            faces[i] = Some(if opt.is_empty() {
                crate::fonts::SANS[i].to_owned()
            } else {
                opt.clone()
            });
        }
        let set = FontSet {
            mono: (!want.0.is_empty()).then(|| want.0.clone()),
            mono_size: size,
            sans: faces,
            symbols: system_symbols(),
            families: want.3.clone(),
        };
        let applied = backend.set_fonts(&set);
        for path in applied.errors {
            self.error(format!("cannot read font {path}"));
        }
        self.view.faces = applied.faces;
        self.view.fonts = Some(want);
        self.view.mono_cell = Some(crate::nodeview::cell_size(backend.shaper(), size));
        // The monospace cell is derived from these fonts.  Force the next
        // frame to measure it again before building Lua's node tree; otherwise
        // a rebuilt view receives the old `ctx.cell` forever.
        self.view.cell = None;
        self.view.screen_size = (0, 0);
        self.view.screen_cols = 0;
        // Chat items made with the old fonts are made again.
        self.lua.views_rev += 1;
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


//! Drawing the GUI with egui: the session list beside the Lua-laid-out
//! screen (`screen.rs`), questions as they come, and Lua styles as egui text
//! formats. Lua-produced lines are monospace, so a column here is a column
//! in the TUI.

use bone_ui::style::{Color, Modifier, Style};
use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{self, Color32, FontId, Key, ScrollArea, Stroke};
use serde_json::Value;

use crate::{Gui, Row};

impl Gui {
    /// Draw one frame into `ui` (the whole window).
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let _ = self.repaint.set(ui.ctx().clone());
        self.pump();
        self.route_keys(ui.ctx());
        self.watch_config();
        // Keep polling the config while idle.
        ui.ctx().request_repaint_after(crate::lua::Watcher::EVERY);
        egui::Panel::left("sessions")
            .resizable(true)
            .default_size(220.0)
            .show(ui, |ui| self.sessions_ui(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| self.screen_ui(ui));
        self.question_ui(ui.ctx());
    }

    fn sessions_ui(&mut self, ui: &mut egui::Ui) {
        if ui.button("New chat").clicked() {
            self.new_chat();
        }
        ui.separator();
        let current = self.chats[self.current].session_id().map(str::to_owned);
        let mut open = None;
        ScrollArea::vertical().show(ui, |ui| {
            for s in &self.sessions {
                let title = s.title.as_deref().unwrap_or("[untitled]");
                let selected = current.as_deref() == Some(s.session_id.as_str());
                if ui.selectable_label(selected, title).clicked() {
                    open = Some(s.session_id.clone());
                }
            }
        });
        if let Some(id) = open {
            self.open_session(&id);
        }
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
            ui.label(text);
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

/// A chat row as text in its styles, one font for all of it.
pub(crate) fn row_job(row: &Row, font: &FontId, visuals: &egui::Visuals) -> LayoutJob {
    let mut job = LayoutJob::default();
    for (text, style) in row {
        job.append(text, 0.0, format(style, font, visuals));
    }
    job
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

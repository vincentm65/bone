//! Drawing the GUI with egui, in the bare style: the session list, the
//! transcript, the prompt and a status line, and questions as they come.
//! Lua-produced lines are monospace, so a column here is a column in the
//! TUI.

use eframe::egui::{
    self, Color32, FontId, Key, KeyboardShortcut, Modifiers, RichText, ScrollArea, TextEdit,
    TextStyle,
};
use serde_json::Value;

use crate::Gui;

impl Gui {
    /// Draw one frame into `ui` (the whole window).
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let _ = self.repaint.set(ui.ctx().clone());
        self.pump();
        egui::Panel::left("sessions")
            .resizable(true)
            .default_size(220.0)
            .show(ui, |ui| self.sessions_ui(ui));
        egui::Panel::bottom("prompt").show(ui, |ui| self.prompt_ui(ui));
        egui::CentralPanel::default().show(ui, |ui| self.chat_ui(ui));
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

    fn chat_ui(&mut self, ui: &mut egui::Ui) {
        let font = FontId::monospace(TextStyle::Monospace.resolve(ui.style()).size);
        let col = ui.ctx().fonts_mut(|f| f.glyph_width(&font, 'M')).max(1.0);
        let cols = (ui.available_width() / col).floor() as usize;
        let rows = self.layout_chat(cols);
        let row_height = ui.text_style_height(&TextStyle::Monospace);
        ui.spacing_mut().item_spacing.y = 0.0;
        let chat = &self.chats[self.current];
        ScrollArea::vertical()
            .auto_shrink(false)
            .stick_to_bottom(true)
            .show_rows(ui, row_height, rows, |ui, range| {
                for row in chat.rows().skip(range.start).take(range.len()) {
                    ui.label(RichText::new(row).font(font.clone()));
                }
            });
    }

    fn prompt_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        let running = self.running();
        let edit = TextEdit::multiline(&mut self.draft)
            .font(TextStyle::Monospace)
            .desired_rows(2)
            .desired_width(f32::INFINITY)
            .hint_text("Message bone (Enter sends, Shift+Enter for a new line)")
            // Enter sends; Shift+Enter is the newline.
            .return_key(KeyboardShortcut::new(Modifiers::SHIFT, Key::Enter));
        let response = ui.add(edit);
        if response.has_focus() && ui.input(|i| i.key_pressed(Key::Enter) && !i.modifiers.shift) {
            self.submit();
        }
        ui.horizontal(|ui| {
            if running {
                ui.spinner();
                ui.label("working…");
            }
            if let Some(s) = &self.status {
                let color = if s.error {
                    ui.visuals().error_fg_color
                } else {
                    Color32::GRAY
                };
                ui.label(RichText::new(&s.text).color(color));
            }
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

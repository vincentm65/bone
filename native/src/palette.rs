//! Searchable keyboard-first navigation. Actions retain stable task identities.
use eframe::egui;

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Task(u64),
    Recent(i64),
    New,
    Command { tab: u64, name: String },
}

pub struct Entry {
    pub label: String,
    pub detail: String,
    pub action: Action,
}

#[derive(Default)]
pub struct Palette {
    pub open: bool,
    pub commands: bool,
    pub tab: u64,
    query: String,
    selected: usize,
    focus: bool,
}

impl Palette {
    pub fn open(&mut self, commands: bool, tab: u64) {
        *self = Self {
            open: true,
            commands,
            tab,
            focus: true,
            ..Default::default()
        };
    }

    pub fn show(&mut self, ctx: &egui::Context, entries: &[Entry]) -> Option<Action> {
        if !self.open {
            return None;
        }
        let mut chosen = None;
        let response =
            crate::surface::modal(ctx, egui::Id::new("task-command-palette")).show(ctx, |ui| {
                ui.set_width(
                    480.0_f32
                        .min(ui.ctx().content_rect().width() - 40.0)
                        .max(180.0),
                );
                ui.heading(if self.commands {
                    "Commands"
                } else {
                    "Switch task"
                });
                let input = ui.add(
                    egui::TextEdit::singleline(&mut self.query)
                        .hint_text("Search…")
                        .desired_width(f32::INFINITY),
                );
                if self.focus {
                    input.request_focus();
                    self.focus = false;
                }
                if input.changed() {
                    self.selected = 0;
                }
                let query = self.query.to_lowercase();
                let matches: Vec<_> = entries
                    .iter()
                    .filter(|e| matches_query(e, &query))
                    .collect();
                if !matches.is_empty() {
                    if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown))
                    {
                        self.selected = (self.selected + 1) % matches.len();
                    }
                    if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)) {
                        self.selected = (self.selected + matches.len() - 1) % matches.len();
                    }
                    self.selected = self.selected.min(matches.len() - 1);
                    if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
                        chosen = Some(matches[self.selected].action.clone());
                    }
                }
                egui::ScrollArea::vertical()
                    .max_height(360.0)
                    .show(ui, |ui| {
                        for (index, entry) in matches.iter().enumerate() {
                            let response = ui
                                .selectable_label(index == self.selected, &entry.label)
                                .on_hover_text(&entry.detail);
                            if index == self.selected {
                                response.scroll_to_me(Some(egui::Align::Center));
                            }
                            if response.clicked() {
                                chosen = Some(entry.action.clone());
                            }
                        }
                        if matches.is_empty() {
                            ui.weak("No matches");
                        }
                    });
                ui.weak("↑ ↓ navigate · Enter select · Esc close");
            });
        if response.should_close() || chosen.is_some() {
            self.open = false;
        }
        chosen
    }
}

fn matches_query(entry: &Entry, query: &str) -> bool {
    entry.label.to_lowercase().contains(query) || entry.detail.to_lowercase().contains(query)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keyboard_navigation_selects_and_escape_closes() {
        let ctx = egui::Context::default();
        let mut palette = Palette::default();
        palette.open(false, 7);
        let entries: Vec<_> = [7, 9]
            .into_iter()
            .map(|id| Entry {
                label: format!("Task {id}"),
                detail: String::new(),
                action: Action::Task(id),
            })
            .collect();
        let frame = |palette: &mut Palette, key: Option<egui::Key>| {
            let mut input = egui::RawInput::default();
            if let Some(key) = key {
                input.events.push(egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                });
            }
            let mut action = None;
            let mut output = ctx.run_ui(input, |ui| {
                action = palette.show(ui.ctx(), &entries);
            });
            output.textures_delta.clear();
            action
        };
        frame(&mut palette, None);
        frame(&mut palette, Some(egui::Key::ArrowDown));
        assert_eq!(
            frame(&mut palette, Some(egui::Key::Enter)),
            Some(Action::Task(9))
        );
        assert!(!palette.open);
        palette.open(false, 7);
        frame(&mut palette, None);
        frame(&mut palette, Some(egui::Key::Escape));
        assert!(!palette.open);
    }

    #[test]
    fn search_matches_labels_and_workspace_without_changing_identity() {
        let entry = Entry {
            label: "Fix Sidebar".into(),
            detail: "/projects/bone".into(),
            action: Action::Task(42),
        };
        assert!(matches_query(&entry, "sidebar"));
        assert!(matches_query(&entry, "bone"));
        assert!(!matches_query(&entry, "absent"));
        assert_eq!(entry.action, Action::Task(42));
    }
}

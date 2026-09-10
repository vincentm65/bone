//! Task model selection is separate from shared provider configuration.
use super::*;
use std::collections::BTreeSet;

#[derive(Default)]
pub struct Picker {
    origin: Option<u64>,
    provider: String,
    search: String,
    custom: String,
}

impl DesktopApp {
    pub(super) fn choose_task_model(&mut self, tab_id: u64, provider: &str, model: &str) -> bool {
        let Some(tab) = self
            .tabs
            .iter_mut()
            .find(|tab| tab.id == tab_id && tab.connected && tab.state.ready && !tab.demo)
        else {
            self.provider_notice = "Connect this task before choosing its model.".into();
            return false;
        };
        if tab.state.busy {
            self.provider_notice = "Wait for this task to finish before changing its model.".into();
            return false;
        }
        let model = model.trim();
        if model.is_empty() {
            self.provider_notice = "Enter a model ID.".into();
            return false;
        }
        let Some(saved) = self
            .config
            .as_ref()
            .and_then(|config| config.providers.iter().find(|p| p.id == provider))
        else {
            self.provider_notice = "Choose a configured provider first.".into();
            return false;
        };
        let command = if tab.host_api_version >= 3 {
            RuntimeCommand::SetConversationModel {
                provider_id: provider.into(),
                model: model.into(),
            }
        } else if saved.model == model {
            RuntimeCommand::SwitchProvider {
                provider_id: provider.into(),
            }
        } else {
            self.provider_notice = "Update the server to select a custom model for a task (API 3 required). Configured models remain available.".into();
            return false;
        };
        let sent = tab.command(command);
        // Success needs no notice: the "Current:" line above reflects the
        // daemon-confirmed model once the snapshot arrives.
        self.provider_notice = if sent {
            String::new()
        } else {
            "The task connection was lost. Reconnect before trying again.".into()
        };
        sent
    }

    pub(super) fn provider_dialog(&mut self, ctx: &egui::Context) {
        if self.demo || !self.show_provider {
            return;
        }
        let mut open = true;
        let target = self
            .provider_origin_tab
            .or_else(|| self.tabs.get(self.selected).map(|t| t.id));
        let Some(tab_id) = target else {
            self.show_provider = false;
            return;
        };
        let Some(index) = self.tabs.iter().position(|t| t.id == tab_id) else {
            self.show_provider = false;
            return;
        };
        if self.model_picker.origin != Some(tab_id) {
            self.model_picker = Picker {
                origin: Some(tab_id),
                provider: self.tabs[index].state.snapshot.provider_id.clone(),
                ..Default::default()
            };
        }
        crate::surface::Surface::new("Choose model", "Select a model for the current task.")
            .size(620.0, 680.0).body_scroll(true)
            .show(ctx, &mut open, |ui| {
                ui.label(format!("For this task: {}", self.tabs[index].title()));
                let snapshot = &self.tabs[index].state.snapshot;
                ui.strong(format!("Current: {} · {}", snapshot.provider_model, snapshot.provider_id));
                ui.weak("Applies to this task. Shared provider defaults are saved separately below.");
                let Some(config) = self.config.clone() else { ui.spinner(); ui.label("Loading configured models…"); return; };
                if config.providers.is_empty() {
                    ui.label("No providers configured.");
                    if ui.button("Set up a provider").clicked() { self.show_setup = true; }
                    return;
                }
                if !config.providers.iter().any(|p| p.id == self.model_picker.provider) {
                    self.model_picker.provider = config.providers[0].id.clone();
                }
                egui::ComboBox::from_id_salt("model-provider").selected_text(
                    config.providers.iter().find(|p| p.id == self.model_picker.provider).map(|p| p.label.as_str()).unwrap_or("Provider")
                ).show_ui(ui, |ui| {
                    for provider in &config.providers {
                        ui.selectable_value(&mut self.model_picker.provider, provider.id.clone(), &provider.label);
                    }
                });
                let provider = self.model_picker.provider.clone();
                let saved = config.providers.iter().find(|p| p.id == provider).unwrap();
                let mut models = BTreeSet::new();
                models.insert(saved.model.clone());
                for meta in &self.conversations {
                    if meta.provider == provider && !meta.model.is_empty() { models.insert(meta.model.clone()); }
                }
                for tab in &self.tabs {
                    if tab.state.snapshot.provider_id == provider && !tab.state.snapshot.provider_model.is_empty() {
                        models.insert(tab.state.snapshot.provider_model.clone());
                    }
                }
                ui.add(egui::TextEdit::singleline(&mut self.model_picker.search).hint_text("Search models").desired_width(ui.available_width()));
                ui.weak("Configured and previously used models");
                let can_choose = self.tabs[index].connected && self.tabs[index].state.ready && !self.tabs[index].state.busy;
                if !can_choose { ui.weak("Connect this task and wait for its current work to finish."); }
                let query = self.model_picker.search.to_lowercase();
                egui::ScrollArea::vertical().id_salt("known-models").max_height(220.0).show(ui, |ui| {
                    let mut found = false;
                    for model in models.iter().filter(|m| m.to_lowercase().contains(&query)) {
                        found = true;
                        let active = self.tabs[index].state.snapshot.provider_id == provider && self.tabs[index].state.snapshot.provider_model == *model;
                        let label = if *model == saved.model { format!("{model} · saved default") } else { model.clone() };
                        if ui.add_enabled(can_choose, egui::Button::selectable(active, label)).clicked() {
                            self.choose_task_model(tab_id, &provider, model);
                        }
                    }
                    if !found { ui.weak("No matching models. Use a custom model ID below."); }
                });
                ui.collapsing("Custom model ID", |ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.model_picker.custom).hint_text("Model ID supported by this provider").desired_width(ui.available_width()));
                    if ui.add_enabled(can_choose && !self.model_picker.custom.trim().is_empty(), egui::Button::new("Use for this task")).clicked() {
                        self.choose_task_model(tab_id, &provider, &self.model_picker.custom.clone());
                    }
                });
                ui.collapsing("Saved provider defaults · shared", |ui| {
                    ui.label("Changes here are shared with other clients. Saving also updates this task when it uses this provider.");
                    if self.model_field_provider != provider {
                        self.model_field_provider = provider.clone();
                        self.model_field = saved.model.clone();
                    }
                    ui.label("Default model ID");
                    ui.add(egui::TextEdit::singleline(&mut self.model_field).desired_width(ui.available_width()));
                    if ui.add_enabled(can_choose && !self.model_field.trim().is_empty() && self.model_field.trim() != saved.model, egui::Button::new("Save provider default")).clicked() {
                        self.save_model_for_provider(Some(tab_id), Some(&provider), self.model_field.trim().into());
                    }
                });
                if !self.provider_notice.is_empty() { ui.label(&self.provider_notice); }
            });
        self.show_provider = open;
    }
}

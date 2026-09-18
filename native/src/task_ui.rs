//! Task navigation and actions retain the identity of the task that invoked them.
use super::*;

const TASK_CONTROL_HEIGHT: f32 = 36.0;
const TASK_CONTROL_RADIUS: u8 = 8;
const TASK_CONTROL_INSET: f32 = 12.0;
const TASK_CONTROL_TEXT_INSET: f32 = 36.0;

/// Match the desktop's quiet surfaces while keeping the primary action easy to find.
pub(super) fn new_task_button(ui: &mut egui::Ui) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), TASK_CONTROL_HEIGHT),
        egui::Sense::click(),
    );
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), "New task")
    });
    if ui.is_rect_visible(rect) {
        let bg = ui.visuals().panel_fill;
        let fg = ui.visuals().text_color();
        let accent = ui.visuals().hyperlink_color;
        let fill = if response.is_pointer_button_down_on() {
            bg.lerp_to_gamma(accent, 0.22)
        } else if response.hovered() {
            bg.lerp_to_gamma(accent, 0.13)
        } else {
            bg.lerp_to_gamma(fg, 0.075)
        };
        let border = if response.has_focus() {
            accent
        } else {
            bg.lerp_to_gamma(fg, 0.12)
        };
        ui.painter().rect(
            rect,
            TASK_CONTROL_RADIUS,
            fill,
            egui::Stroke::new(1.0, border),
            egui::StrokeKind::Inside,
        );
        let center = egui::pos2(rect.left() + TASK_CONTROL_INSET + 7.0, rect.center().y);
        let stroke = egui::Stroke::new(1.5, fg);
        for offset in [egui::vec2(5.0, 0.0), egui::vec2(0.0, 5.0)] {
            ui.painter()
                .line_segment([center - offset, center + offset], stroke);
        }
        ui.painter().text(
            egui::pos2(rect.left() + TASK_CONTROL_TEXT_INSET, rect.center().y),
            egui::Align2::LEFT_CENTER,
            "New task",
            egui::FontId::new(14.0, egui::FontFamily::Name("semibold".into())),
            fg,
        );
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A full-row search target with a vector icon and the native text editor's input behavior.
pub(super) fn task_search(ui: &mut egui::Ui, query: &mut String) {
    let (rect, container) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), TASK_CONTROL_HEIGHT),
        egui::Sense::CLICK,
    );
    // Reserve the background before the editor so it stays behind text and selection.
    let background = ui.painter().add(egui::Shape::Noop);
    let text_rect = egui::Rect::from_min_max(
        egui::pos2(rect.left() + TASK_CONTROL_TEXT_INSET, rect.top()),
        egui::pos2(rect.right() - TASK_CONTROL_INSET, rect.bottom()),
    );
    let editor = ui.place(
        text_rect,
        egui::TextEdit::singleline(query)
            .id(ui.make_persistent_id("task-search"))
            .hint_text(
                egui::RichText::new("Search tasks")
                    .size(14.0)
                    .color(ui.visuals().weak_text_color()),
            )
            .font(egui::FontId::proportional(14.0))
            .frame(egui::Frame::NONE)
            .margin(0)
            .vertical_align(egui::Align::Center)
            .desired_width(text_rect.width()),
    );
    if container.clicked() {
        editor.request_focus();
    }
    container.on_hover_cursor(egui::CursorIcon::Text);
    let focused = ui.memory(|memory| memory.has_focus(editor.id));
    let hovered = ui.rect_contains_pointer(rect);
    let bg = ui.visuals().panel_fill;
    let fg = ui.visuals().text_color();
    let border = if focused {
        ui.visuals().hyperlink_color
    } else {
        bg.lerp_to_gamma(fg, if hovered { 0.22 } else { 0.12 })
    };
    ui.painter().set(
        background,
        egui::epaint::RectShape::new(
            rect,
            TASK_CONTROL_RADIUS,
            bg.lerp_to_gamma(fg, if focused || hovered { 0.05 } else { 0.025 }),
            egui::Stroke::new(1.0, border),
            egui::StrokeKind::Inside,
        ),
    );
    let center = egui::pos2(
        rect.left() + TASK_CONTROL_INSET + 6.0,
        rect.center().y - 1.0,
    );
    let stroke = egui::Stroke::new(1.5, ui.visuals().weak_text_color());
    ui.painter().circle_stroke(center, 4.5, stroke);
    ui.painter().line_segment(
        [center + egui::vec2(3.5, 3.5), center + egui::vec2(7.0, 7.0)],
        stroke,
    );
}

pub struct CommandInput {
    tab: u64,
    name: String,
    arguments: String,
    focus: bool,
}

fn matches_task(title: &str, detail: &str, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    title.to_lowercase().contains(&query) || detail.to_lowercase().contains(&query)
}

fn recent_title(meta: &ConversationMeta) -> String {
    if meta.full_title.trim().is_empty() {
        if meta.title.trim().is_empty() {
            format!("Task {}", meta.id)
        } else {
            meta.title.clone()
        }
    } else {
        meta.full_title.clone()
    }
}

fn recent_context(meta: &ConversationMeta) -> String {
    let timestamp = if meta.updated_at_local.trim().is_empty() {
        &meta.updated_at
    } else {
        &meta.updated_at_local
    };
    // 12-hour clock time; fall back to the raw ISO time part (or the whole stamp)
    // so a malformed timestamp still shows something.
    let time = time_of_day(timestamp)
        .or_else(|| timestamp.split_once('T').map(|(_, time)| time.to_owned()))
        .unwrap_or_else(|| timestamp.clone());
    [time.as_str(), meta.model.as_str()]
        .into_iter()
        .filter(|value| !value.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

fn recent_group(meta: &ConversationMeta) -> &'static str {
    match relative_date(&meta.updated_at, &meta.updated_at_local).as_str() {
        "yesterday" => "Yesterday",
        value if value == "today" || value.ends_with("am") || value.ends_with("pm") => "Today",
        _ => "Earlier",
    }
}

/// 12-hour `h:mm` with an `am`/`pm` suffix drawn from an ISO timestamp's time
/// part (`...THH:MM:SS`), if well-formed.
fn time_of_day(iso: &str) -> Option<String> {
    let hhmm = iso.get(11..16)?;
    let bytes = hhmm.as_bytes();
    if bytes.get(2) != Some(&b':')
        || !hhmm
            .char_indices()
            .all(|(i, c)| i == 2 || c.is_ascii_digit())
    {
        return None;
    }
    let hour: u32 = hhmm.get(..2)?.parse().ok()?;
    if hour > 23 {
        return None;
    }
    let minute = &hhmm[3..5];
    let (hour, suffix) = match hour {
        0 => (12, "am"),
        1..=11 => (hour, "am"),
        12 => (12, "pm"),
        _ => (hour - 12, "pm"),
    };
    Some(format!("{hour}:{minute}{suffix}"))
}

/// Human relative label for a conversation timestamp. The day buckets are
/// computed from the UTC `iso` instant; for same-day entries the daemon-local
/// `iso_local` supplies the `hh:mm` clock time, falling back to "today" when
/// absent (e.g. against an older daemon).
pub(super) fn relative_date(iso: &str, iso_local: &str) -> String {
    let parsed = (|| -> Option<(i64, i64, i64)> {
        Some((
            iso.get(..4)?.parse().ok()?,
            iso.get(5..7)?.parse().ok()?,
            iso.get(8..10)?.parse().ok()?,
        ))
    })();
    let Some((year, month, day)) =
        parsed.filter(|(_, m, d)| (1..=12).contains(m) && (1..=31).contains(d))
    else {
        return iso.into();
    };
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let days = era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / 86400;
    match now as i64 - days {
        0 => time_of_day(iso_local).unwrap_or_else(|| "today".into()),
        1 => "yesterday".into(),
        n @ 2..=13 => format!("{n} days ago"),
        _ => iso.get(..10).unwrap_or(iso).into(),
    }
}

impl Tab {
    /// Execute a command entered in its own dialog without touching the prompt draft.
    pub(super) fn run_separate_command(&mut self, name: &str, arguments: &str) -> bool {
        if !self.connected || !self.state.ready || self.state.busy || self.pending_command.is_some()
        {
            return false;
        }
        let draft = Draft {
            text: std::mem::take(&mut self.composer),
            pastes: std::mem::take(&mut self.pastes),
            attachments: std::mem::take(&mut self.attachments),
        };
        let autocomplete = self.autocomplete.take();
        let history = self.history_index.take();
        let sent =
            self.handle_builtin_command(name, arguments) || self.run_command(name, arguments);
        self.composer = draft.text;
        self.pastes = draft.pastes;
        self.attachments = draft.attachments;
        self.autocomplete = autocomplete;
        self.history_index = history;
        sent
    }
}

impl DesktopApp {
    pub(super) fn tool_permissions_section(&mut self, ui: &mut egui::Ui) {
        let auto = self.approval_mode() == "danger";
        ui.strong("Tool permissions");
        ui.weak("Applies to all tasks on this server");
        ui.add_enabled_ui(self.has_connected_tab(), |ui| {
            if ui
                .selectable_label(!auto, "Ask before running tools")
                .clicked()
            {
                self.set_approval_mode("safe");
                ui.close();
            }
            if ui.selectable_label(auto, "Auto-approve tools").clicked() {
                self.set_approval_mode("danger");
                ui.close();
            }
        });
    }
    pub(super) fn sync_task_titles(&mut self) {
        for tab in &mut self.tabs {
            tab.saved_title = self
                .conversations
                .iter()
                .find(|meta| tab.conversation_id == Some(meta.id))
                .map(|meta| (meta.id, recent_title(meta)));
        }
    }

    pub(super) fn sidebar_lists(&mut self, ui: &mut egui::Ui) {
        self.sync_task_titles();
        ui.add_space(4.0);
        ui.label(egui::RichText::new("Open").weak().size(12.0));
        self.open_tabs(ui);
        if !self.demo {
            ui.add_space(8.0);
            ui.label(egui::RichText::new("Recent").weak().size(12.0));
            if self.conversations_request.is_some() {
                ui.weak("Loading tasks…");
            } else if !self.conversations_loaded {
                ui.weak("Waiting for the daemon…");
            } else {
                let rows: Vec<_> = self
                    .conversations
                    .iter()
                    .filter(|meta| {
                        !self
                            .tabs
                            .iter()
                            .any(|tab| tab.conversation_id == Some(meta.id))
                    })
                    .filter(|meta| matches_task(&recent_title(meta), "", &self.history_search))
                    .cloned()
                    .collect();
                if rows.is_empty() {
                    ui.weak(if self.history_search.trim().is_empty() {
                        "No recent tasks."
                    } else {
                        "No matching recent tasks."
                    });
                }
                for group in ["Today", "Yesterday", "Earlier"] {
                    let grouped: Vec<_> = rows
                        .iter()
                        .filter(|meta| recent_group(meta) == group)
                        .collect();
                    if grouped.is_empty() {
                        continue;
                    }
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new(group).small().weak());
                    for meta in grouped {
                        let title = recent_title(meta);
                        let response = task_row(
                            ui,
                            RowIndicator::None,
                            egui::Color32::GRAY,
                            egui::RichText::new(one_line(&title)),
                            false,
                        )
                        .on_hover_text(format!(
                            "{title}\n{} messages · {}",
                            meta.message_count, meta.updated_at_local
                        ));
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(recent_context(meta)).small().weak(),
                            )
                            .truncate()
                            .selectable(false),
                        );
                        response.context_menu(|ui| {
                            if ui.button("Rename…").clicked() {
                                self.start_rename(meta.id);
                                ui.close();
                            }
                            if ui.button("Delete…").clicked() {
                                self.start_delete(meta.id);
                                ui.close();
                            }
                        });
                        if response.clicked() {
                            self.open_conversation(meta.id, &ui.ctx().clone());
                        }
                    }
                }
            }
        }
        if !self.sidebar_notice.is_empty() {
            ui.colored_label(ui.visuals().warn_fg_color, &self.sidebar_notice);
        }
    }

    pub(super) fn open_tabs(&mut self, ui: &mut egui::Ui) {
        let mut shown = 0;
        for index in 0..self.tabs.len() {
            let tab = &self.tabs[index];
            let title = tab.title();
            if !matches_task(&title, &tab.workspace, &self.history_search) {
                continue;
            }
            shown += 1;
            let (indicator, color) = tab.status_indicator();
            let status = tab.navigation_status().0;
            let location = self.conversation_location(tab.id);
            let show_location = self.workspace.windows.len() > 1
                || self
                    .workspace
                    .window(self.window_ui.rendering)
                    .is_some_and(|window| window.root.panes().len() > 1);
            let private = if tab.state.snapshot.incognito {
                "Incognito · "
            } else {
                ""
            };
            let response = task_row(
                ui,
                indicator,
                color,
                egui::RichText::new(format!("{private}{title}").trim()),
                self.selected == index,
            )
            .on_hover_text(format!("{title}\n{location}\n{status}"));
            let tab_id = tab.id;
            let conversation = tab.conversation_id;
            let demo = tab.demo;
            if show_location {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!("{location} · {status}"))
                            .small()
                            .weak(),
                    )
                    .truncate()
                    .selectable(false),
                );
            }
            response.context_menu(|ui| {
                ui.add_enabled_ui(!demo && conversation.is_some(), |ui| {
                    if ui.button("Rename…").clicked() {
                        self.start_rename(conversation.unwrap());
                        ui.close();
                    }
                    if ui.button("Delete…").clicked() {
                        self.start_delete(conversation.unwrap());
                        ui.close();
                    }
                });
                if !demo && ui.button("Close").clicked() {
                    self.close_tab(index);
                    ui.close();
                }
                if !demo {
                    self.tab_context_menu(tab_id, ui);
                }
            });
            if response.clicked() {
                self.focus_conversation(tab_id, ui.ctx());
                self.note_layout_change(ui.ctx());
            }
        }
        if shown == 0 {
            ui.weak(if self.history_search.trim().is_empty() {
                "No open tasks."
            } else {
                "No matching open tasks."
            });
        }
    }

    pub(super) fn start_rename(&mut self, id: i64) {
        let title = self
            .conversations
            .iter()
            .find(|meta| meta.id == id)
            .map(recent_title)
            .or_else(|| {
                self.tabs
                    .iter()
                    .find(|tab| tab.conversation_id == Some(id))
                    .map(Tab::title)
            });
        let Some(title) = title else {
            return;
        };
        self.rename_field = title;
        self.rename_target = Some(id);
        self.delete_target = None;
        self.sidebar_notice.clear();
    }
    pub(super) fn cancel_rename(&mut self) {
        self.rename_target = None;
        self.rename_field.clear();
    }
    pub(super) fn commit_rename(&mut self) {
        let Some(id) = self.rename_target else {
            return;
        };
        let title = self.rename_field.trim().to_owned();
        if title.is_empty() {
            self.sidebar_notice = "A task title cannot be blank.".into();
            return;
        }
        if self.request_conversation_mutation(HostRequest::ConversationRename {
            id,
            title,
            limit: 0,
        }) {
            self.cancel_rename();
        } else {
            self.sidebar_notice = "Cannot rename right now: connect to the server and wait for the current task update.".into();
        }
    }
    pub(super) fn start_delete(&mut self, id: i64) {
        if self.pending_delete.is_some() {
            return;
        }
        let title = self
            .conversations
            .iter()
            .find(|meta| meta.id == id)
            .map(recent_title)
            .or_else(|| {
                self.tabs
                    .iter()
                    .find(|tab| tab.conversation_id == Some(id))
                    .map(Tab::title)
            })
            .unwrap_or_else(|| format!("Task {id}"));
        self.cancel_rename();
        self.delete_target = Some((id, title));
        self.sidebar_notice.clear();
    }
    pub(super) fn cancel_delete(&mut self) {
        self.pending_delete = None;
        self.delete_target = None;
    }
    pub(super) fn commit_delete(&mut self) {
        let Some((id, _)) = self.delete_target.clone() else {
            return;
        };
        if !self.has_connected_tab() || self.conversations_request.is_some() {
            self.sidebar_notice = "Cannot delete right now: connect to the server and wait for the current task update.".into();
            return;
        }
        let open: Vec<_> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(_, tab)| tab.conversation_id == Some(id))
            .map(|(i, _)| i)
            .collect();
        if open.is_empty() {
            if self.request_conversation_mutation(HostRequest::ConversationDelete { id, limit: 0 })
            {
                self.delete_target = None;
            }
        } else {
            self.pending_delete = Some(id);
            for index in open {
                self.finish_close_tab(index);
            }
        }
    }
    pub(super) fn poll_task_delete(&mut self, ctx: &egui::Context) {
        let Some(id) = self.pending_delete else {
            return;
        };
        // Keep a transport available even when deleting the last open task.
        if !self.tabs.iter().any(|tab| !tab.closing && !tab.remove) {
            self.add_tab(Intent::New, ctx);
            if !self.demo {
                self.connect_index(self.selected);
            }
        }
        if self.tabs.iter().any(|tab| tab.conversation_id == Some(id)) {
            return;
        }
        if self.request_conversation_mutation(HostRequest::ConversationDelete { id, limit: 0 }) {
            self.pending_delete = None;
            self.delete_target = None;
        }
    }

    pub(super) fn task_dialogs(&mut self, ctx: &egui::Context) {
        if self.rename_target.is_some() {
            let response =
                crate::surface::modal(ctx, egui::Id::new("rename-task")).show(ctx, |ui| {
                    ui.set_max_width((ctx.content_rect().width() - 48.0).clamp(180.0, 420.0));
                    ui.heading("Rename task");
                    let input = ui.add(
                        egui::TextEdit::singleline(&mut self.rename_field)
                            .desired_width(ui.available_width()),
                    );
                    if !ui.memory(|m| m.has_focus(input.id)) && ui.memory(|m| m.focused().is_none())
                    {
                        input.request_focus();
                    }
                    let enter = input.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.horizontal(|ui| {
                        if ui.button("Save").clicked() || enter {
                            self.commit_rename();
                        }
                        if ui.button("Cancel").clicked() {
                            self.cancel_rename();
                        }
                    });
                    if !self.sidebar_notice.is_empty() {
                        ui.label(&self.sidebar_notice);
                    }
                });
            if response.should_close() {
                self.cancel_rename();
            }
        }
        if let Some((id, title)) = self.delete_target.clone() {
            let response = crate::surface::modal(ctx, egui::Id::new("delete-task")).show(ctx, |ui| {
                ui.set_max_width((ctx.content_rect().width() - 48.0).clamp(180.0, 420.0));
                ui.heading("Delete task?");
                ui.label(title);
                ui.label("Permanently deletes saved messages and usage for this task.");
                let open = self.tabs.iter().any(|tab| tab.conversation_id == Some(id));
                if open { ui.label("This also closes the task, stops its current turn, and discards its draft and attachments."); }
                if self.pending_delete.is_some() {
                    ui.spinner();
                    ui.label("Closing the task and waiting for a connection to delete its saved history…");
                    if ui.button("Cancel deletion; keep saved history").clicked() { self.cancel_delete(); }
                } else {
                    ui.horizontal(|ui| {
                        if ui.button(if open { "Close and delete" } else { "Delete task" }).clicked() { self.commit_delete(); }
                        if ui.button("Cancel").clicked() { self.cancel_delete(); }
                    });
                }
                if !self.sidebar_notice.is_empty() { ui.label(&self.sidebar_notice); }
            });
            if response.should_close() {
                self.cancel_delete();
            }
        }
    }

    pub(super) fn run_palette_action(&mut self, tab: u64, name: &str, ctx: &egui::Context) {
        let Some(index) = self.tabs.iter().position(|t| t.id == tab) else {
            return;
        };
        self.focus_conversation(tab, ctx);
        match name {
            "config" => self.apply_ui_request(tab, UiRequest::OpenConfig),
            "stats" => self.apply_ui_request(tab, UiRequest::OpenStats),
            "setup" => self.apply_ui_request(tab, UiRequest::OpenSetup),
            "catalog" => self.apply_ui_request(tab, UiRequest::OpenCatalog),
            "provider" | "model" => self.apply_ui_request(tab, UiRequest::OpenProvider),
            "edit" | "e" => self.tabs[index].open_editor(),
            "new" | "clear" => {
                self.apply_shortcut(egui::Key::T, ctx);
            }
            "incognito" => {
                self.set_incognito(!self.tabs[index].state.snapshot.incognito);
            }
            "help" => {
                let advertised = self.tabs[index]
                    .state
                    .frontend
                    .as_ref()
                    .map(|f| f.commands.as_slice())
                    .unwrap_or(&[]);
                let help = commands::help(advertised);
                self.tabs[index].state.push_row("system", help);
            }
            _ => {
                self.command_input = Some(CommandInput {
                    tab,
                    name: name.into(),
                    arguments: String::new(),
                    focus: true,
                })
            }
        }
    }

    pub(super) fn command_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut input) = self.command_input.take() else {
            return;
        };
        let Some(index) = self.tabs.iter().position(|t| t.id == input.tab) else {
            return;
        };
        let mut close = false;
        let response =
            crate::surface::modal(ctx, egui::Id::new("command-arguments")).show(ctx, |ui| {
                ui.set_width((ctx.content_rect().width() - 48.0).clamp(180.0, 420.0));
                ui.heading(format!("/{}", input.name));
                ui.weak(format!("Task: {}", self.tabs[index].title()));
                let field = ui.add(
                    egui::TextEdit::singleline(&mut input.arguments)
                        .hint_text("Arguments (optional)")
                        .desired_width(ui.available_width()),
                );
                if input.focus {
                    field.request_focus();
                    input.focus = false;
                }
                let tab = &mut self.tabs[index];
                let enabled = tab.connected
                    && tab.state.ready
                    && !tab.state.busy
                    && tab.pending_command.is_none();
                if !enabled {
                    ui.weak("Connect this task and wait for its current work to finish.");
                }
                ui.horizontal(|ui| {
                    let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if enabled && (ui.button("Run command").clicked() || enter) {
                        close = tab.run_separate_command(&input.name, &input.arguments);
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });
        if !close && !response.should_close() {
            self.command_input = Some(input);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{recent_context, relative_date, time_of_day};
    use crate::ConversationMeta;

    fn sample_meta(updated_at: &str, updated_at_local: &str) -> ConversationMeta {
        ConversationMeta {
            id: 1,
            title: "Task".into(),
            full_title: String::new(),
            updated_at: updated_at.into(),
            updated_at_local: updated_at_local.into(),
            message_count: 1,
            provider: "openai".into(),
            model: "gpt-5".into(),
        }
    }

    #[test]
    fn recent_context_uses_twelve_hour_clock() {
        assert_eq!(
            recent_context(&sample_meta("2026-09-10T19:07:43Z", "2026-09-10T19:07:43")),
            "7:07pm · gpt-5"
        );
        // Empty local stamps fall back to the UTC `updated_at` time part.
        assert_eq!(
            recent_context(&sample_meta("2026-09-10T08:05:00Z", "")),
            "8:05am · gpt-5"
        );
    }

    fn utc_date(days: i64) -> (i64, i64, i64) {
        let z = days + 719468;
        let era = z.div_euclid(146097);
        let doe = z - era * 146097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        (y + i64::from(m <= 2), m, d)
    }

    fn today_utc() -> (i64, i64, i64) {
        let days = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            / 86400;
        utc_date(days as i64)
    }

    #[test]
    fn time_of_day_formats_twelve_hour_clock_and_rejects_malformed() {
        assert_eq!(time_of_day("2026-09-10T08:05:00"), Some("8:05am".into()));
        assert_eq!(time_of_day("2026-09-10T19:07:43Z"), Some("7:07pm".into()));
        assert_eq!(time_of_day("2026-09-10T00:00:00"), Some("12:00am".into()));
        assert_eq!(time_of_day("2026-09-10T12:00:00"), Some("12:00pm".into()));
        assert_eq!(time_of_day("2026-09-10T13:59:00"), Some("1:59pm".into()));
        assert_eq!(time_of_day(""), None);
        assert_eq!(time_of_day("2026-09-10"), None);
        assert_eq!(time_of_day("2026-09-10T0a:05"), None);
        assert_eq!(time_of_day("2026-09-10T:0:05"), None);
        assert_eq!(time_of_day("2026-09-10T25:00:00"), None);
    }

    #[test]
    fn today_entries_show_local_clock_time() {
        let (y, m, d) = today_utc();
        let iso = format!("{y:04}-{m:02}-{d:02}T12:00:00Z");
        let local = format!("{y:04}-{m:02}-{d:02}T08:30:00");
        assert_eq!(relative_date(&iso, &local), "8:30am");
    }

    #[test]
    fn today_falls_back_to_today_without_local_time() {
        let (y, m, d) = today_utc();
        let iso = format!("{y:04}-{m:02}-{d:02}T12:00:00Z");
        assert_eq!(relative_date(&iso, ""), "today");
        assert_eq!(relative_date(&iso, "not-a-timestamp"), "today");
    }

    #[test]
    fn older_entries_keep_day_buckets_and_plain_dates() {
        // Far past: falls through to the ISO date prefix.
        assert_eq!(
            relative_date("2000-01-01T00:00:00Z", ""),
            "2000-01-01".to_string()
        );
        // Malformed date: echoes the input unchanged.
        assert_eq!(relative_date("nope", ""), "nope");
    }
}

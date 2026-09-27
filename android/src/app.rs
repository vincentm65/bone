//! The phone UI: a connect screen, then one conversation with a slide-out chat
//! list, pending approvals above the input, and a Send/Stop button. Sized and
//! spaced for touch.

use bone_protocol::RuntimeCommand;
use eframe::egui;

use crate::link::{Link, LinkEvent, Target};
use crate::state::{RowKind, State};

const TEXT_SIZE: f32 = 16.0;
const SMALL_SIZE: f32 = 13.0;
/// Minimum height of anything tappable.
const TOUCH: f32 = 44.0;

pub struct PhoneApp {
    /// SSH host typed on the connect screen.
    host: String,
    link: Option<Link>,
    target: Option<Target>,
    connected: bool,
    /// Why the last connection ended or failed.
    error: String,
    state: State,
    input: String,
    drawer_open: bool,
}

impl PhoneApp {
    /// `target` connects immediately; `None` starts on the connect screen.
    pub fn new(ctx: &egui::Context, target: Option<Target>) -> Self {
        apply_style(ctx);
        let host = match &target {
            Some(Target::Ssh(host)) => host.clone(),
            _ => String::new(),
        };
        let mut app = Self {
            host,
            link: None,
            target: None,
            connected: false,
            error: String::new(),
            state: State::default(),
            input: String::new(),
            drawer_open: false,
        };
        if let Some(target) = target {
            app.connect(ctx, target);
        }
        app
    }

    fn connect(&mut self, ctx: &egui::Context, target: Target) {
        self.error.clear();
        self.connected = false;
        self.state = State::default();
        self.link = Some(Link::open(target.clone(), ctx.clone()));
        self.target = Some(target);
    }

    fn send(&mut self, command: RuntimeCommand) {
        if let Some(link) = &self.link
            && !link.send(command)
        {
            self.error = "connection closed".into();
        }
    }

    fn pump(&mut self) {
        let Some(link) = &self.link else {
            return;
        };
        let mut replies = Vec::new();
        for event in link.drain() {
            match event {
                LinkEvent::Connected => {
                    self.connected = true;
                    replies.push(RuntimeCommand::NewConversation);
                    replies.push(self.state.request_conversations());
                }
                LinkEvent::Disconnected(reason) => {
                    self.connected = false;
                    self.error = reason;
                }
                LinkEvent::Runtime(event) => replies.extend(self.state.apply(event)),
            }
        }
        if !self.connected && !self.error.is_empty() {
            self.link = None;
        }
        for command in replies {
            self.send(command);
        }
    }

    fn title(&self) -> String {
        self.state
            .conversation_id
            .and_then(|id| self.state.conversations.iter().find(|meta| meta.id == id))
            .map(|meta| meta.title.clone())
            .filter(|title| !title.trim().is_empty() && title != "(new)")
            .unwrap_or_else(|| "New chat".into())
    }

    fn connect_screen(&mut self, ui: &mut egui::Ui) {
        let connecting = self.link.is_some();
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(ui.available_height() * 0.2);
            ui.vertical_centered(|ui| {
                ui.heading("Bone");
                ui.add_space(16.0);
                ui.label("SSH host (a ~/.ssh/config alias or user@host)");
                let field = ui.add_enabled(
                    !connecting,
                    egui::TextEdit::singleline(&mut self.host)
                        .hint_text("devbox")
                        .desired_width(f32::INFINITY),
                );
                let submitted = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.add_space(8.0);
                let label = if connecting {
                    "Connecting…"
                } else {
                    "Connect"
                };
                let button = ui.add_enabled(
                    !connecting && !self.host.trim().is_empty(),
                    egui::Button::new(label).min_size(egui::vec2(160.0, TOUCH)),
                );
                if (button.clicked() || submitted) && !connecting && !self.host.trim().is_empty() {
                    let host = self.host.trim().to_string();
                    self.connect(ui.ctx(), Target::Ssh(host));
                }
                if !self.error.is_empty() {
                    ui.add_space(12.0);
                    ui.colored_label(ui.visuals().error_fg_color, &self.error);
                }
            });
        });
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("top-bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.set_min_height(TOUCH);
                if ui
                    .add(egui::Button::new("☰").min_size(egui::vec2(TOUCH, TOUCH)))
                    .clicked()
                {
                    self.drawer_open = !self.drawer_open;
                    if self.drawer_open {
                        let command = self.state.request_conversations();
                        self.send(command);
                    }
                }
                ui.vertical(|ui| {
                    ui.add(egui::Label::new(egui::RichText::new(self.title()).strong()).truncate());
                    let target = self.target.as_ref().map(Target::label).unwrap_or_default();
                    let detail = if self.state.model.is_empty() {
                        target
                    } else {
                        format!("{target} · {}", self.state.model)
                    };
                    ui.add(egui::Label::new(egui::RichText::new(detail).small().weak()).truncate());
                });
            });
        });
    }

    fn drawer(&mut self, ui: &mut egui::Ui) {
        let width = (ui.available_width() * 0.85).min(340.0);
        let mut open = None;
        let mut new_chat = false;
        egui::Panel::left("chats")
            .exact_size(width)
            .resizable(false)
            .show(ui, |ui| {
                if ui
                    .add(
                        egui::Button::new("+ New chat")
                            .min_size(egui::vec2(ui.available_width(), TOUCH)),
                    )
                    .clicked()
                {
                    new_chat = true;
                }
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for meta in &self.state.conversations {
                        let current = self.state.conversation_id == Some(meta.id);
                        let when = meta
                            .updated_at_local
                            .get(..16)
                            .unwrap_or(&meta.updated_at)
                            .replace('T', " ");
                        let text = format!("{}\n{when} · {} msgs", meta.title, meta.message_count);
                        let response = ui.add(
                            egui::Button::selectable(current, text)
                                .min_size(egui::vec2(ui.available_width(), TOUCH))
                                .truncate(),
                        );
                        if response.clicked() {
                            open = Some(meta.id);
                        }
                    }
                });
            });
        if new_chat {
            self.drawer_open = false;
            self.send(RuntimeCommand::NewConversation);
        }
        if let Some(id) = open {
            self.drawer_open = false;
            let command = self.state.open(id);
            self.send(command);
        }
    }

    fn composer(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("composer").show(ui, |ui| {
            ui.add_space(4.0);
            if let Some(approval) = self.state.approvals.first().cloned() {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.label(egui::RichText::new(format!("Allow {}?", approval.name)).strong());
                    ui.label(egui::RichText::new(&approval.summary).monospace());
                    if let Some(reason) = &approval.blocked {
                        ui.colored_label(ui.visuals().warn_fg_color, reason);
                    }
                    ui.horizontal(|ui| {
                        let allow = ui.add_enabled(
                            approval.blocked.is_none(),
                            egui::Button::new("Allow").min_size(egui::vec2(96.0, TOUCH)),
                        );
                        let deny =
                            ui.add(egui::Button::new("Deny").min_size(egui::vec2(96.0, TOUCH)));
                        let answer = if allow.clicked() {
                            self.state.answer(true)
                        } else if deny.clicked() {
                            self.state.answer(false)
                        } else {
                            None
                        };
                        if let Some(command) = answer {
                            self.send(command);
                        }
                    });
                });
                ui.add_space(4.0);
            }
            ui.horizontal(|ui| {
                let button_width = 72.0;
                ui.add(
                    egui::TextEdit::multiline(&mut self.input)
                        .hint_text("Message")
                        .desired_rows(2)
                        .desired_width(ui.available_width() - button_width - 8.0),
                );
                if self.state.busy {
                    if ui
                        .add(egui::Button::new("Stop").min_size(egui::vec2(button_width, TOUCH)))
                        .clicked()
                    {
                        self.send(RuntimeCommand::Cancel);
                    }
                } else if ui
                    .add_enabled(
                        !self.input.trim().is_empty(),
                        egui::Button::new("Send").min_size(egui::vec2(button_width, TOUCH)),
                    )
                    .clicked()
                    && let Some(command) = self.state.submit(&self.input)
                {
                    self.input.clear();
                    self.send(command);
                }
            });
            ui.add_space(4.0);
        });
    }

    fn transcript(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    let visuals = ui.visuals().clone();
                    for row in &self.state.rows {
                        match row.kind {
                            RowKind::User => {
                                egui::Frame::new()
                                    .fill(visuals.faint_bg_color)
                                    .corner_radius(8)
                                    .inner_margin(8)
                                    .show(ui, |ui| {
                                        ui.set_width(ui.available_width());
                                        ui.add(egui::Label::new(&row.text).selectable(true));
                                    });
                            }
                            RowKind::Assistant => {
                                ui.add(egui::Label::new(&row.text).selectable(true));
                            }
                            RowKind::Tool => {
                                let color = if row.failed {
                                    visuals.error_fg_color
                                } else {
                                    visuals.weak_text_color()
                                };
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(format!("▸ {}", row.text))
                                            .small()
                                            .monospace()
                                            .color(color),
                                    )
                                    .truncate(),
                                );
                            }
                            RowKind::Note => {
                                ui.label(egui::RichText::new(&row.text).small().weak().italics());
                            }
                            RowKind::Error => {
                                ui.colored_label(visuals.error_fg_color, &row.text);
                            }
                        }
                        ui.add_space(6.0);
                    }
                    if self.state.busy {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.weak(if self.state.status.is_empty() {
                                "Working…"
                            } else {
                                &self.state.status
                            });
                        });
                    }
                    if !self.error.is_empty() {
                        ui.colored_label(visuals.error_fg_color, &self.error);
                    }
                });
        });
    }
}

impl eframe::App for PhoneApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.pump();
        if !self.connected {
            self.connect_screen(ui);
            return;
        }
        self.top_bar(ui);
        if self.drawer_open {
            self.drawer(ui);
        }
        self.composer(ui);
        self.transcript(ui);
    }
}

fn apply_style(ctx: &egui::Context) {
    ctx.all_styles_mut(|style| {
        for (text_style, size) in [
            (egui::TextStyle::Body, TEXT_SIZE),
            (egui::TextStyle::Button, TEXT_SIZE),
            (egui::TextStyle::Monospace, TEXT_SIZE - 1.0),
            (egui::TextStyle::Small, SMALL_SIZE),
            (egui::TextStyle::Heading, 24.0),
        ] {
            if let Some(font) = style.text_styles.get_mut(&text_style) {
                font.size = size;
            }
        }
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(12.0, 8.0);
        style.spacing.interact_size.y = TOUCH;
    });
}

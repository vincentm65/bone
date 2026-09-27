//! The phone UI: a connect screen, then one conversation with a slide-out chat
//! list, pending approvals above the input, and a Send/Stop button. Sized and
//! spaced for touch.

use std::path::PathBuf;
use std::sync::Arc;

use bone_protocol::RuntimeCommand;
use eframe::egui;

use crate::link::{Link, LinkEvent, Target};
use crate::ssh::{Destination, Identity};
use crate::state::{RowKind, State};

const TEXT_SIZE: f32 = 16.0;
const SMALL_SIZE: f32 = 13.0;
/// Minimum height of anything tappable.
const TOUCH: f32 = 44.0;
/// Last-used `user@host` and bone path, one per line, in the data directory.
const SETTINGS_FILE: &str = "connection";

pub struct PhoneApp {
    data_dir: PathBuf,
    /// The app's SSH key, or why it could not be loaded.
    identity: Result<Arc<Identity>, String>,
    /// `user@host[:port]` typed on the connect screen.
    destination: String,
    /// Path of `bone` on the computer.
    bone: String,
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
    /// `data_dir` holds the app's SSH key, pinned host keys, and settings.
    pub fn new(ctx: &egui::Context, target: Option<Target>, data_dir: PathBuf) -> Self {
        apply_style(ctx);
        let identity = Identity::load_or_create(&data_dir)
            .map(Arc::new)
            .map_err(|error| format!("could not create this app's SSH key: {error}"));
        let saved = std::fs::read_to_string(data_dir.join(SETTINGS_FILE)).unwrap_or_default();
        let mut lines = saved.lines();
        let mut app = Self {
            data_dir,
            identity,
            destination: lines.next().unwrap_or_default().to_string(),
            bone: lines.next().unwrap_or("bone").to_string(),
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
        let identity = match &self.identity {
            Ok(identity) => identity.clone(),
            Err(error) => {
                self.error = error.clone();
                return;
            }
        };
        if let Target::Ssh { destination, bone } = &target {
            self.destination = format!(
                "{}@{}:{}",
                destination.user, destination.host, destination.port
            );
            self.bone = bone.clone();
            let _ = std::fs::write(
                self.data_dir.join(SETTINGS_FILE),
                format!("{}\n{}\n", self.destination, self.bone),
            );
        }
        self.error.clear();
        self.connected = false;
        self.state = State::default();
        self.link = Some(Link::open(target.clone(), identity, ctx.clone()));
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
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.add_space(24.0);
                ui.heading("Bone");
                ui.add_space(16.0);
                ui.label("Computer (user@host or user@host:port)");
                ui.add_enabled(
                    !connecting,
                    egui::TextEdit::singleline(&mut self.destination)
                        .hint_text("me@192.168.1.20")
                        .desired_width(f32::INFINITY),
                );
                ui.label("Path to bone on the computer");
                ui.add_enabled(
                    !connecting,
                    egui::TextEdit::singleline(&mut self.bone)
                        .hint_text("bone")
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(8.0);
                let label = if connecting {
                    "Connecting…"
                } else {
                    "Connect"
                };
                let ready = !connecting && !self.destination.trim().is_empty();
                if ui
                    .add_enabled(
                        ready,
                        egui::Button::new(label).min_size(egui::vec2(160.0, TOUCH)),
                    )
                    .clicked()
                {
                    match Destination::parse(&self.destination) {
                        Ok(destination) => {
                            let bone = match self.bone.trim() {
                                "" => "bone".to_string(),
                                path => path.to_string(),
                            };
                            self.connect(ui.ctx(), Target::Ssh { destination, bone });
                        }
                        Err(error) => self.error = error,
                    }
                }
                if !self.error.is_empty() {
                    ui.add_space(8.0);
                    ui.colored_label(ui.visuals().error_fg_color, &self.error);
                }
                if let Ok(identity) = &self.identity {
                    ui.add_space(24.0);
                    ui.label("Add this app's key to ~/.ssh/authorized_keys on the computer:");
                    let mut line = identity.public_line();
                    ui.add(
                        egui::TextEdit::multiline(&mut line)
                            .font(egui::TextStyle::Monospace)
                            .desired_rows(3)
                            .desired_width(f32::INFINITY),
                    );
                    if ui
                        .add(egui::Button::new("Copy key").min_size(egui::vec2(120.0, TOUCH)))
                        .clicked()
                    {
                        ui.ctx().copy_text(identity.public_line());
                    }
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

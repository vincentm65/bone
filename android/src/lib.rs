//! Bone on Android: the desktop app's own UI (`bone_desktop::DesktopApp`),
//! attached to the daemon on your computer over in-app SSH running
//! `bone stdio` there.
//!
//! This crate adds only what a phone needs around that UI: the SSH connector
//! ([`ssh`]), a connect screen shown until a link is up (and again if it
//! fails), and the Android entry point. The same code runs in a phone-sized
//! desktop window (`bone-android-preview`) for development without a device.

pub mod ssh;

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use bone_desktop::DesktopApp;
use bone_desktop::connection::{Connector, Stream, Target};
use eframe::egui;

use crate::ssh::{Destination, Identity};

/// Last-used `user@host[:port]` and bone path, one per line, in the data dir.
const SETTINGS_FILE: &str = "connection";

/// Opens `<bone> stdio` on the computer over in-app SSH, once per chat tab.
struct SshConnector {
    identity: Arc<Identity>,
    destination: Destination,
    bone: String,
}

impl Connector for SshConnector {
    fn label(&self) -> String {
        format!("ssh: {}", self.destination.host)
    }

    fn connect(&self) -> Pin<Box<dyn Future<Output = Result<Stream, String>> + Send>> {
        let identity = self.identity.clone();
        let destination = self.destination.clone();
        let bone = self.bone.clone();
        Box::pin(async move {
            let (stream, session) = ssh::open(&identity, &destination, &bone).await?;
            let (read, write) = tokio::io::split(stream);
            Ok(Stream {
                read: Box::new(read),
                write: Box::new(write),
                guard: Box::new(session),
            })
        })
    }
}

/// The connect screen until a link is up, then the desktop UI.
pub struct Launcher {
    data_dir: PathBuf,
    /// The app's SSH key, or why it could not be loaded.
    identity: Result<Arc<Identity>, String>,
    destination: String,
    bone: String,
    error: String,
    app: Option<DesktopApp>,
}

impl Launcher {
    /// `data_dir` holds the app's SSH key, pinned host keys, and settings.
    pub fn new(ctx: &egui::Context, data_dir: PathBuf) -> Self {
        bone_desktop::install_look(ctx);
        let identity = Identity::load_or_create(&data_dir)
            .map(Arc::new)
            .map_err(|error| format!("could not create this app's SSH key: {error}"));
        let saved = std::fs::read_to_string(data_dir.join(SETTINGS_FILE)).unwrap_or_default();
        let mut lines = saved.lines();
        Self {
            destination: lines.next().unwrap_or_default().to_string(),
            bone: lines.next().unwrap_or("bone").to_string(),
            data_dir,
            identity,
            error: String::new(),
            app: None,
        }
    }

    fn connect(&mut self, ctx: &egui::Context) {
        let identity = match &self.identity {
            Ok(identity) => identity.clone(),
            Err(error) => {
                self.error = error.clone();
                return;
            }
        };
        let destination = match Destination::parse(&self.destination) {
            Ok(destination) => destination,
            Err(error) => {
                self.error = error;
                return;
            }
        };
        let bone = match self.bone.trim() {
            "" => "bone".to_string(),
            path => path.to_string(),
        };
        let _ = std::fs::write(
            self.data_dir.join(SETTINGS_FILE),
            format!("{}\n{bone}\n", self.destination.trim()),
        );
        self.error.clear();
        let connector = SshConnector {
            identity,
            destination,
            bone,
        };
        self.app = Some(DesktopApp::remote(
            ctx.clone(),
            Target::Custom(Arc::new(connector)),
        ));
    }

    fn connect_screen(&mut self, ui: &mut egui::Ui) {
        let frame = egui::Frame::new()
            .fill(ui.visuals().panel_fill)
            .inner_margin(16);
        egui::CentralPanel::default().frame(frame).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.heading("bone");
                ui.add_space(12.0);
                ui.weak("Computer (user@host or user@host:port)");
                ui.add(
                    egui::TextEdit::singleline(&mut self.destination)
                        .hint_text("me@100.64.0.1")
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(4.0);
                ui.weak("Path to bone on the computer");
                ui.add(
                    egui::TextEdit::singleline(&mut self.bone)
                        .hint_text("bone")
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(8.0);
                if ui
                    .add_enabled(
                        !self.destination.trim().is_empty(),
                        egui::Button::new("Connect"),
                    )
                    .clicked()
                {
                    self.connect(ui.ctx());
                }
                if !self.error.is_empty() {
                    ui.add_space(8.0);
                    ui.colored_label(ui.visuals().error_fg_color, &self.error);
                }
                if let Ok(identity) = &self.identity {
                    ui.add_space(20.0);
                    ui.weak("Add this key to ~/.ssh/authorized_keys on the computer:");
                    let mut line = identity.public_line();
                    ui.add(
                        egui::TextEdit::multiline(&mut line)
                            .font(egui::TextStyle::Monospace)
                            .desired_rows(3)
                            .desired_width(f32::INFINITY),
                    );
                    if ui.button("Copy key").clicked() {
                        ui.ctx().copy_text(identity.public_line());
                    }
                }
            });
        });
    }
}

impl eframe::App for Launcher {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if let Some(app) = &mut self.app {
            match app.connection_failure() {
                // Back to the connect screen, with the reason.
                Some(reason) => {
                    self.error = reason.to_string();
                    self.app = None;
                    bone_desktop::install_look(ui.ctx());
                }
                None => {
                    app.ui(ui, frame);
                    return;
                }
            }
        }
        self.connect_screen(ui);
    }
}

/// Run the app in a phone-sized desktop window.
pub fn run_preview(data_dir: PathBuf) -> eframe::Result {
    eframe::run_native(
        "Bone (phone preview)",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([400.0, 820.0])
                .with_min_inner_size([320.0, 480.0]),
            ..Default::default()
        },
        Box::new(move |cc| Ok(Box::new(Launcher::new(&cc.egui_ctx, data_dir)))),
    )
}

/// Android entry point, called by the NativeActivity glue.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: winit::platform::android::activity::AndroidApp) {
    let data_dir = app
        .internal_data_path()
        .unwrap_or_else(|| PathBuf::from("."));
    let result = eframe::run_native(
        "Bone",
        eframe::NativeOptions {
            android_app: Some(app),
            renderer: eframe::Renderer::Glow,
            ..Default::default()
        },
        Box::new(move |cc| Ok(Box::new(Launcher::new(&cc.egui_ctx, data_dir)))),
    );
    if let Err(error) = result {
        eprintln!("bone-android: {error}");
    }
}

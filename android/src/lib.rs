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
/// The daemon's last theme payload, applied at startup so the default theme
/// never flashes before the daemon's arrives.
const THEME_FILE: &str = "theme.json";
/// With no cached theme, how long to wait for the daemon's before showing the
/// UI anyway.
const THEME_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

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

/// The visible part of the window in physical pixels as
/// `[left, top, right, bottom]` (on Android: above the on-screen keyboard).
pub type VisibleArea = Box<dyn Fn() -> [i32; 4]>;

/// The connect screen until a link is up, then the desktop UI, both laid out
/// inside the window's visible area.
pub struct Launcher {
    /// `None` uses the whole window (the desktop preview).
    visible_area: Option<VisibleArea>,
    /// The area laid out last frame, to notice the keyboard opening or closing.
    last_area: egui::Rect,
    /// The focused text field's last keyboard request (see [`Launcher::ui`]).
    last_ime: Option<egui::output::IMEOutput>,
    data_dir: PathBuf,
    /// The app's SSH key, or why it could not be loaded.
    identity: Result<Arc<Identity>, String>,
    destination: String,
    bone: String,
    error: String,
    /// The last theme payload seen, cached in [`THEME_FILE`].
    theme: Option<serde_json::Value>,
    /// When the current connection attempt started.
    connecting_since: Option<std::time::Instant>,
    app: Option<DesktopApp>,
}

impl Launcher {
    /// `data_dir` holds the app's SSH key, pinned host keys, and settings.
    pub fn new(ctx: &egui::Context, data_dir: PathBuf, visible_area: Option<VisibleArea>) -> Self {
        let theme = std::fs::read(data_dir.join(THEME_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        restyle(ctx, theme.as_ref());
        let identity = Identity::load_or_create(&data_dir)
            .map(Arc::new)
            .map_err(|error| format!("could not create this app's SSH key: {error}"));
        let saved = std::fs::read_to_string(data_dir.join(SETTINGS_FILE)).unwrap_or_default();
        let mut lines = saved.lines();
        Self {
            visible_area,
            last_area: egui::Rect::NOTHING,
            last_ime: None,
            destination: lines.next().unwrap_or_default().to_string(),
            bone: lines.next().unwrap_or("bone").to_string(),
            data_dir,
            identity,
            error: String::new(),
            theme,
            connecting_since: None,
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
        let mut app = DesktopApp::remote(ctx.clone(), Target::Custom(Arc::new(connector)));
        if let Some(theme) = &self.theme {
            app.set_theme(ctx, theme.clone());
        }
        self.app = Some(app);
        self.connecting_since = Some(std::time::Instant::now());
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

impl Launcher {
    /// Where to lay out the UI: the window minus whatever covers it.
    fn area(&self, ctx: &egui::Context, screen: egui::Rect) -> egui::Rect {
        let Some(visible_area) = &self.visible_area else {
            return screen;
        };
        let [left, top, right, bottom] = visible_area();
        if right <= left || bottom <= top {
            return screen;
        }
        let points = |px: i32| px as f32 / ctx.pixels_per_point();
        egui::Rect::from_min_max(
            egui::pos2(points(left), points(top)),
            egui::pos2(points(right), points(bottom)),
        )
        .intersect(screen)
    }

    fn content(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if let Some(app) = &mut self.app {
            match app.connection_failure() {
                // Back to the connect screen, with the reason.
                Some(reason) => {
                    self.error = reason.to_string();
                    self.app = None;
                    restyle(ui.ctx(), self.theme.as_ref());
                }
                None => {
                    let waited = self
                        .connecting_since
                        .is_some_and(|since| since.elapsed() >= THEME_WAIT);
                    if app.theme().is_some() || waited {
                        eframe::App::ui(app, ui, frame);
                    } else {
                        // First launch, nothing cached: connect unseen so the
                        // default theme never shows.
                        ui.scope_builder(egui::UiBuilder::new().invisible(), |ui| {
                            eframe::App::ui(app, ui, frame)
                        });
                        ui.painter().text(
                            ui.max_rect().center(),
                            egui::Align2::CENTER_CENTER,
                            "Connecting…",
                            egui::TextStyle::Body.resolve(ui.style()),
                            ui.visuals().weak_text_color(),
                        );
                        ui.ctx()
                            .request_repaint_after(std::time::Duration::from_millis(100));
                    }
                    if let Some(theme) = app.theme()
                        && self.theme.as_ref() != Some(theme)
                    {
                        let _ = std::fs::write(self.data_dir.join(THEME_FILE), theme.to_string());
                        self.theme = Some(theme.clone());
                    }
                    return;
                }
            }
        }
        self.connect_screen(ui);
    }
}

/// The app's fonts and style, in `theme` when one is cached.
fn restyle(ctx: &egui::Context, theme: Option<&serde_json::Value>) {
    bone_desktop::install_look(ctx);
    if let Some(theme) = theme {
        bone_desktop::install_theme(ctx, theme);
    }
}

impl eframe::App for Launcher {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let screen = ui.max_rect();
        let area = self.area(ui.ctx(), screen);
        // Android does not announce the keyboard opening or closing, so keep
        // checking while it may be up or the area is still settling.
        if self.visible_area.is_some()
            && (area != self.last_area || area != screen || ui.ctx().egui_wants_keyboard_input())
        {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(150));
        }
        self.last_area = area;
        ui.painter()
            .rect_filled(screen, 0.0, ui.visuals().panel_fill);
        ui.scope_builder(egui::UiBuilder::new().max_rect(area), |ui| {
            self.content(ui, frame)
        });
        // egui requests the keyboard only for a focused text field drawn in
        // view. When a layout shift (live pane, keyboard animation) pushes the
        // input out of view for one frame, Android would hide and reshow the
        // keyboard; keep the request while a field still has focus.
        let ctx = ui.ctx();
        match ctx.output(|output| output.ime) {
            // Replays must not repeat a one-off keyboard reset.
            Some(ime) => {
                self.last_ime = Some(egui::output::IMEOutput {
                    should_interrupt_composition: false,
                    ..ime
                })
            }
            None if ctx.memory(|memory| memory.focused().is_some()) => {
                ctx.output_mut(|output| output.ime = self.last_ime);
            }
            None => self.last_ime = None,
        }
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
        Box::new(move |cc| Ok(Box::new(Launcher::new(&cc.egui_ctx, data_dir, None)))),
    )
}

/// Android entry point, called by the NativeActivity glue.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: winit::platform::android::activity::AndroidApp) {
    let data_dir = app
        .internal_data_path()
        .unwrap_or_else(|| PathBuf::from("."));
    // NativeActivity resizes its content view (not the surface) for the
    // on-screen keyboard and reports it only as the content rect.
    let content = app.clone();
    let visible_area: VisibleArea = Box::new(move || {
        let rect = content.content_rect();
        [rect.left, rect.top, rect.right, rect.bottom]
    });
    let result = eframe::run_native(
        "Bone",
        eframe::NativeOptions {
            android_app: Some(app),
            renderer: eframe::Renderer::Glow,
            ..Default::default()
        },
        Box::new(move |cc| {
            Ok(Box::new(Launcher::new(
                &cc.egui_ctx,
                data_dir,
                Some(visible_area),
            )))
        }),
    );
    if let Err(error) = result {
        eprintln!("bone-android: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Top edge of the first painted text containing `needle`.
    fn text_top(output: &egui::FullOutput, needle: &str) -> Option<f32> {
        output
            .shapes
            .iter()
            .find_map(|clipped| match &clipped.shape {
                egui::Shape::Text(text) if text.galley.job.text.contains(needle) => {
                    Some(text.pos.y)
                }
                _ => None,
            })
    }

    #[test]
    fn the_ui_is_laid_out_inside_the_visible_area() {
        let dir = std::env::temp_dir().join(format!("bone-android-area-{}", std::process::id()));
        let ctx = egui::Context::default();
        // Visible from y=300 to y=500 of an 800pt-tall window.
        let area: VisibleArea = Box::new(|| [0, 300, 400, 500]);
        let mut launcher = Launcher::new(&ctx, dir.clone(), Some(area));
        let mut output = None;
        for _ in 0..2 {
            if let Some(previous) = output.as_mut() {
                let previous: &mut egui::FullOutput = previous;
                previous.textures_delta.clear();
            }
            output = Some(ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(400.0, 800.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    let screen = ui.max_rect();
                    let area = launcher.area(ui.ctx(), screen);
                    ui.scope_builder(egui::UiBuilder::new().max_rect(area), |ui| {
                        launcher.connect_screen(ui)
                    });
                },
            ));
        }
        let mut output = output.unwrap();
        output.textures_delta.clear();
        let top = text_top(&output, "Computer").expect("connect screen");
        assert!((300.0..500.0).contains(&top), "laid out at {top}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_cached_theme_styles_the_app_from_the_first_frame() {
        let dir = std::env::temp_dir().join(format!("bone-android-theme-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let theme = serde_json::json!({ "palette": { "bg": "#102030", "fg": "#eeeeee" } });
        std::fs::write(dir.join(THEME_FILE), theme.to_string()).unwrap();
        let ctx = egui::Context::default();
        Launcher::new(&ctx, dir.clone(), None);
        assert_eq!(
            ctx.style_of(ctx.theme()).visuals.panel_fill,
            egui::Color32::from_rgb(0x10, 0x20, 0x30)
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}

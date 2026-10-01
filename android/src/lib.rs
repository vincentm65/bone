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

use bone_desktop::connection::{Connector, Stream, Target};
use bone_desktop::{DesktopApp, WorkspaceState};
use eframe::egui;

use crate::ssh::{Destination, Identity};

/// Last-used `user@host[:port]` and bone path, one per line, in the data dir.
const SETTINGS_FILE: &str = "connection";
/// The daemon's last theme payload, applied at startup so the default theme
/// never flashes before the daemon's arrives.
const THEME_FILE: &str = "theme.json";
/// The frontend-owned open-chat workspace, restored after Android restarts.
const WORKSPACE_FILE: &str = "workspace.json";
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
            let (stream, session, exit) = ssh::open(&identity, &destination, &bone).await?;
            let (read, write) = tokio::io::split(stream);
            Ok(Stream {
                read: Box::new(read),
                write: Box::new(write),
                guard: Box::new(session),
                failure: Some(Box::new(move || exit.describe())),
            })
        })
    }
}

/// The visible part of the window in physical pixels as
/// `[left, top, right, bottom]` (on Android: above the on-screen keyboard).
pub type VisibleArea = Box<dyn Fn() -> [i32; 4]>;

/// Turn connection failures into a short next step while retaining the
/// original error for diagnostics and security details.
fn friendly_connection_error(error: &str) -> String {
    let lower = error.to_ascii_lowercase();
    let guidance = if lower.contains("host key") && lower.contains("changed") {
        "The computer's SSH host key changed. Verify the computer before forgetting its pinned key."
    } else if lower.contains("rejected this app's key")
        || lower.contains("login failed")
        || lower.contains("authentication")
    {
        "SSH reached the computer, but it did not accept Bone's key. Add the full one-time public key to ~/.ssh/authorized_keys there."
    } else if lower.contains("could not reach")
        || lower.contains("timed out")
        || lower.contains("connection refused")
        || lower.contains("no route")
    {
        "Bone could not reach the computer. Check that it is awake, the address and port are correct, and SSH is reachable."
    } else if lower.contains("could not run")
        || lower.contains("command not found")
        || lower.contains("remote bone")
        || lower.contains("remote stderr")
    {
        "SSH connected, but Bone did not start on the computer. Check the Bone path under Advanced connection settings."
    } else {
        "Could not connect to the computer."
    };
    format!("{guidance}\n\nDetails: {error}")
}

/// The connect screen until a link is up, then the desktop UI, both laid out
/// inside the window's visible area.
pub struct Launcher {
    /// `None` uses the whole window (the desktop preview).
    visible_area: Option<VisibleArea>,
    /// The area laid out last frame, to notice the keyboard opening or closing.
    last_area: egui::Rect,
    /// The focused text field's last keyboard request (see [`Launcher::ui`]).
    last_ime: Option<egui::output::IMEOutput>,
    /// Focus id associated with `last_ime`; prevents replaying a request for a
    /// field that has since been closed or replaced.
    last_ime_focus: Option<egui::Id>,
    data_dir: PathBuf,
    /// The app's SSH key, or why it could not be loaded.
    identity: Result<Arc<Identity>, String>,
    destination: String,
    bone: String,
    /// Start the first frame by trying the saved computer.
    auto_connect: bool,
    /// Whether to show the optional remote Bone path.
    show_advanced: bool,
    error: String,
    /// The last theme payload seen, cached in [`THEME_FILE`].
    theme: Option<serde_json::Value>,
    /// Open chat tabs and selection restored from [`WORKSPACE_FILE`].
    workspace: WorkspaceState,
    /// The copy button gives explicit feedback instead of silently relying on
    /// the platform clipboard.
    key_copied: bool,
    /// A second tap is required before deleting a pinned host key.
    forget_host_pending: Option<Destination>,
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
        let workspace: WorkspaceState = std::fs::read(data_dir.join(WORKSPACE_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let mut lines = saved.lines();
        let destination = lines.next().unwrap_or_default().to_string();
        let bone = lines.next().unwrap_or("bone").to_string();
        let auto_connect = !destination.trim().is_empty();
        Self {
            visible_area,
            last_area: egui::Rect::NOTHING,
            last_ime: None,
            last_ime_focus: None,
            key_copied: false,
            forget_host_pending: None,
            destination,
            bone,
            auto_connect,
            show_advanced: false,
            data_dir,
            identity,
            error: String::new(),
            theme,
            workspace,
            connecting_since: None,
            app: None,
        }
    }

    fn connect(&mut self, ctx: &egui::Context) {
        self.auto_connect = false;
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
        let _ = ssh::write_atomic(
            &self.data_dir.join(SETTINGS_FILE),
            format!("{}\n{bone}\n", self.destination.trim()).as_bytes(),
        );
        self.error.clear();
        self.forget_host_pending = None;
        let connector = SshConnector {
            identity,
            destination,
            bone,
        };
        let mut app = DesktopApp::remote_with_workspace(
            ctx.clone(),
            Target::Custom(Arc::new(connector)),
            self.workspace.clone(),
        );
        if let Some(theme) = &self.theme {
            app.set_theme(ctx, theme.clone());
        }
        self.app = Some(app);
        self.connecting_since = Some(std::time::Instant::now());
    }

    fn start_saved_connection(&mut self, ctx: &egui::Context) {
        if self.app.is_none() && self.auto_connect {
            self.auto_connect = false;
            self.connect(ctx);
        }
    }

    fn persist_workspace(&mut self, workspace: WorkspaceState) {
        if workspace == self.workspace {
            return;
        }
        let Ok(bytes) = serde_json::to_vec(&workspace) else {
            return;
        };
        if ssh::write_atomic(&self.data_dir.join(WORKSPACE_FILE), &bytes).is_ok() {
            self.workspace = workspace;
        }
    }

    fn connect_screen(&mut self, ui: &mut egui::Ui) {
        // The embedded Android window has no separate navigation bar. A Back
        // press first dismisses the keyboard by surrendering the focused field;
        // only a second Back leaves the launcher. Desktop preview keeps its
        // normal window behavior.
        let android_back = self.visible_area.is_some()
            && ui.input_mut(|input| {
                input.consume_key(egui::Modifiers::NONE, egui::Key::BrowserBack)
            });
        if android_back {
            if let Some(id) = ui.memory(|memory| memory.focused()) {
                ui.memory_mut(|memory| {
                    memory.surrender_focus(id);
                    memory.stop_text_input();
                });
                self.last_ime = None;
                self.last_ime_focus = None;
            } else {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }

        let mut connect = false;
        let mut forget = false;
        let mut cancel_forget = false;
        let frame = egui::Frame::new()
            .fill(ui.visuals().panel_fill)
            .inner_margin(16);
        egui::CentralPanel::default().frame(frame).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                let width = ui.available_width();
                ui.heading("Connect to your computer");
                ui.add_space(12.0);
                ui.weak("Computer address (user@host or user@host:port)");
                let destination = ui.add(
                    egui::TextEdit::singleline(&mut self.destination)
                        .id(egui::Id::new("android-destination"))
                        .hint_text("me@100.64.0.1")
                        .desired_width(f32::INFINITY)
                        .min_size(egui::vec2(0.0, 36.0))
                        .return_key(egui::KeyboardShortcut::new(
                            egui::Modifiers::NONE,
                            egui::Key::Enter,
                        )),
                );
                ui.add_space(8.0);
                ui.checkbox(&mut self.show_advanced, "Advanced connection settings");
                let mut bone_lost_focus = false;
                if self.show_advanced {
                    ui.weak("Only change this if `bone` is not on the SSH PATH.");
                    ui.weak("Path to Bone on the computer");
                    let bone = ui.add(
                        egui::TextEdit::singleline(&mut self.bone)
                            .id(egui::Id::new("android-bone-path"))
                            .hint_text("bone")
                            .font(egui::TextStyle::Monospace)
                            .desired_width(f32::INFINITY)
                            .min_size(egui::vec2(0.0, 36.0))
                            .return_key(egui::KeyboardShortcut::new(
                                egui::Modifiers::NONE,
                                egui::Key::Enter,
                            )),
                    );
                    bone_lost_focus = bone.lost_focus();
                }
                ui.add_space(10.0);
                let can_connect = !self.destination.trim().is_empty();
                let label = if self.error.is_empty() {
                    "Connect to computer"
                } else {
                    "Try again"
                };
                let button = ui
                    .add_enabled_ui(can_connect, |ui| {
                        ui.add_sized([width, 40.0], egui::Button::new(label))
                    })
                    .inner;
                connect = can_connect
                    && (button.clicked()
                        || (ui.input(|input| input.key_pressed(egui::Key::Enter))
                            && (destination.lost_focus() || bone_lost_focus)));

                let parsed = ssh::Destination::parse(&self.destination).ok();
                let pinned = match (self.identity.as_ref(), parsed.as_ref()) {
                    (Ok(identity), Some(destination)) => identity.host_key_pinned(destination),
                    _ => false,
                };
                if pinned {
                    ui.add_space(10.0);
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        format!("Host key pinned for {}.", self.destination.trim()),
                    );
                    ui.weak("A changed host key is refused. Forget it only after verifying the computer.");
                    if self.forget_host_pending.as_ref() == parsed.as_ref() {
                        ui.add_space(4.0);
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            "Forget this pin and trust the next key on reconnect?",
                        );
                        ui.horizontal(|ui| {
                            if ui
                                .add_sized([width * 0.55, 36.0], egui::Button::new("Forget pin"))
                                .clicked()
                            {
                                forget = true;
                            }
                            if ui
                                .add_sized([width * 0.35, 36.0], egui::Button::new("Keep"))
                                .clicked()
                            {
                                cancel_forget = true;
                            }
                        });
                    } else if ui
                        .add_sized([width, 36.0], egui::Button::new("Forget pinned host key"))
                        .clicked()
                    {
                        self.forget_host_pending = parsed.clone();
                    }
                } else if parsed.is_some() {
                    ui.add_space(10.0);
                    ui.weak("The first successful connection pins this computer's host key; changed keys are refused.");
                }

                if !self.error.is_empty() {
                    ui.add_space(10.0);
                    ui.colored_label(ui.visuals().error_fg_color, &self.error);
                }
                if let Err(error) = &self.identity {
                    ui.add_space(8.0);
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                if let Ok(identity) = &self.identity {
                    ui.add_space(20.0);
                    ui.heading("One-time setup");
                    ui.weak("Add this app's key to ~/.ssh/authorized_keys on your computer. You only need to do this once for this computer:");
                    let mut line = identity.public_line();
                    ui.add(
                        egui::TextEdit::multiline(&mut line)
                            .font(egui::TextStyle::Monospace)
                            .desired_rows(3)
                            .desired_width(f32::INFINITY)
                            .interactive(false),
                    );
                    let label = if self.key_copied {
                        "Copied ✓"
                    } else {
                        "Copy key"
                    };
                    if ui
                        .add_sized([width, 40.0], egui::Button::new(label))
                        .clicked()
                    {
                        ui.ctx().copy_text(identity.public_line());
                        self.key_copied = true;
                    }
                    if self.key_copied {
                        ui.weak("Copied to the clipboard. Append the full line to authorized_keys.");
                    }
                }
            });
        });
        if cancel_forget {
            self.forget_host_pending = None;
        }
        if forget {
            let destination = self.forget_host_pending.take();
            match (self.identity.as_ref(), destination) {
                (Ok(identity), Some(destination)) => match identity.forget_host_key(&destination) {
                    Ok(true) => self.error.clear(),
                    Ok(false) => self.error = "No pinned host key was found.".into(),
                    Err(error) => self.error = format!("Could not forget host key: {error}"),
                },
                _ => self.error = "Enter a valid user@host before forgetting its key.".into(),
            }
        }
        if connect {
            self.connect(ui.ctx());
        }
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
        self.start_saved_connection(ui.ctx());
        let Some(app) = self.app.as_mut() else {
            self.connect_screen(ui);
            return;
        };
        if let Some(reason) = app.connection_failure().map(str::to_owned) {
            // Back to the connect screen, with the reason.
            let workspace = app.workspace_state();
            self.error = friendly_connection_error(&reason);
            self.persist_workspace(workspace);
            self.app = None;
            self.last_ime = None;
            self.last_ime_focus = None;
            restyle(ui.ctx(), self.theme.as_ref());
            self.connect_screen(ui);
            return;
        }
        let waited = self
            .connecting_since
            .is_some_and(|since| since.elapsed() >= THEME_WAIT);
        if app.theme().is_some() || waited {
            eframe::App::ui(app, ui, frame);
        } else {
            // First launch, nothing cached: connect unseen so the default
            // theme never shows.
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
            let _ = ssh::write_atomic(
                &self.data_dir.join(THEME_FILE),
                theme.to_string().as_bytes(),
            );
            self.theme = Some(theme.clone());
        }
        let workspace = app.workspace_state();
        self.persist_workspace(workspace);
    }
    fn sync_ime(&mut self, ctx: &egui::Context) {
        let focused = ctx.memory(|memory| memory.focused());
        match (ctx.output(|output| output.ime), focused) {
            (Some(ime), Some(focused)) => {
                // Never repeat a one-off composition reset when Android asks us
                // to replay the keyboard request during a layout animation.
                self.last_ime = Some(egui::output::IMEOutput {
                    should_interrupt_composition: false,
                    ..ime
                });
                self.last_ime_focus = Some(focused);
            }
            (None, Some(focused)) if self.last_ime_focus == Some(focused) => {
                // A visible-area change can make the editor omit IME output for
                // one frame. Keep the same request alive for the same field.
                ctx.output_mut(|output| output.ime = self.last_ime);
            }
            _ => {
                // Focus loss, tab/page closure, or a different editor must not
                // inherit a stale keyboard request.
                self.last_ime = None;
                self.last_ime_focus = None;
            }
        }
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
        self.sync_ime(ui.ctx());
        #[cfg(target_os = "android")]
        copy_to_clipboard(ui.ctx());
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

/// egui's Android backend drops `CopyText`, so hand copied text to the
/// system `ClipboardManager` over JNI.
#[cfg(target_os = "android")]
fn copy_to_clipboard(ctx: &egui::Context) {
    let texts: Vec<String> = ctx.output_mut(|output| {
        let mut texts = Vec::new();
        output.commands.retain(|command| match command {
            egui::OutputCommand::CopyText(text) => {
                texts.push(text.clone());
                false
            }
            _ => true,
        });
        texts
    });
    for text in texts {
        if let Err(err) = set_clipboard(&text) {
            eprintln!("bone: clipboard copy failed: {err}");
        }
    }
}

#[cfg(target_os = "android")]
fn set_clipboard(text: &str) -> jni::errors::Result<()> {
    use jni::objects::{JObject, JValue};
    use jni::{jni_sig, jni_str};

    let android = ndk_context::android_context();
    // SAFETY: android-activity initialises the context with the live VM and a
    // global reference to the activity, both valid for the process lifetime.
    let vm = unsafe { jni::JavaVM::from_raw(android.vm().cast()) };
    vm.attach_current_thread(|env| {
        // SAFETY: see above; `JObject` does not delete the reference on drop.
        let activity = unsafe { JObject::from_raw(env, android.context().cast()) };
        let service = env.new_string("clipboard")?;
        let clipboard = env
            .call_method(
                &activity,
                jni_str!("getSystemService"),
                jni_sig!("(Ljava/lang/String;)Ljava/lang/Object;"),
                &[JValue::Object(&service)],
            )?
            .l()?;
        let label = env.new_string("bone")?;
        let body = env.new_string(text)?;
        let clip = env
            .call_static_method(
                jni_str!("android/content/ClipData"),
                jni_str!("newPlainText"),
                jni_sig!(
                    "(Ljava/lang/CharSequence;Ljava/lang/CharSequence;)Landroid/content/ClipData;"
                ),
                &[JValue::Object(&label), JValue::Object(&body)],
            )?
            .l()?;
        env.call_method(
            &clipboard,
            jni_str!("setPrimaryClip"),
            jni_sig!("(Landroid/content/ClipData;)V"),
            &[JValue::Object(&clip)],
        )?;
        Ok(())
    })
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

    fn connect_frame(
        launcher: &mut Launcher,
        ctx: &egui::Context,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 800.0),
                )),
                events,
                ..Default::default()
            },
            |ui| launcher.connect_screen(ui),
        );
        output.textures_delta.clear();
        output
    }

    fn key_event(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "bone-android-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn saved_destination_starts_auto_connecting() {
        let dir = test_dir("saved");
        std::fs::write(dir.join(SETTINGS_FILE), "me@example.test\nbone\n").unwrap();
        let ctx = egui::Context::default();
        let mut launcher = Launcher::new(&ctx, dir.clone(), None);

        assert!(launcher.auto_connect);
        launcher.start_saved_connection(&ctx);
        assert!(launcher.app.is_some());
        assert!(!launcher.auto_connect);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn empty_destination_keeps_manual_setup() {
        let dir = test_dir("manual");
        let ctx = egui::Context::default();
        let mut launcher = Launcher::new(&ctx, dir.clone(), None);

        assert!(!launcher.auto_connect);
        launcher.start_saved_connection(&ctx);
        assert!(launcher.app.is_none());
        let output = connect_frame(&mut launcher, &ctx, Vec::new());
        assert!(text_top(&output, "Connect to computer").is_some());

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn connection_errors_keep_details_with_friendly_guidance() {
        let refused = friendly_connection_error("could not reach devbox:22: Connection refused");
        assert!(refused.contains("Bone could not reach the computer."));
        assert!(refused.contains("Connection refused"));

        let changed = friendly_connection_error(
            "the host key for devbox changed; if expected, clear the app's known hosts",
        );
        assert!(changed.contains("Verify the computer"));
        assert!(changed.contains("clear the app's known hosts"));
    }

    #[test]
    fn enter_on_the_destination_field_starts_connecting() {
        let dir = test_dir("enter");
        let ctx = egui::Context::default();
        let mut launcher = Launcher::new(&ctx, dir.clone(), None);
        assert!(!launcher.auto_connect);
        launcher.destination = "me@example.test".into();
        launcher.bone = "bone".into();
        let _ = connect_frame(&mut launcher, &ctx, Vec::new());
        ctx.memory_mut(|memory| memory.request_focus(egui::Id::new("android-destination")));
        let _ = connect_frame(&mut launcher, &ctx, vec![key_event(egui::Key::Enter)]);

        assert!(launcher.app.is_some(), "Enter should use the connect path");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_pending_forget_for_another_destination_does_not_confirm() {
        let dir = std::env::temp_dir().join(format!("bone-android-forget-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let ctx = egui::Context::default();
        let mut launcher = Launcher::new(&ctx, dir.clone(), None);
        let known_hosts = dir.join("known_hosts");
        for (host, seed) in [("devbox", 1), ("otherbox", 2)] {
            let key = russh::keys::PrivateKey::from(
                russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&[seed; 32]),
            );
            russh::keys::known_hosts::learn_known_hosts_path(
                host,
                22,
                key.public_key(),
                &known_hosts,
            )
            .unwrap();
        }

        launcher.forget_host_pending = Some(Destination::parse("me@devbox").unwrap());
        launcher.destination = "me@otherbox".into();
        let output = connect_frame(&mut launcher, &ctx, Vec::new());
        assert!(text_top(&output, "Forget pinned host key").is_some());
        assert!(text_top(&output, "Forget this pin").is_none());
        let identity = launcher.identity.as_ref().unwrap();
        assert!(identity.host_key_pinned(&Destination::parse("me@devbox").unwrap()));
        assert!(identity.host_key_pinned(&Destination::parse("me@otherbox").unwrap()));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn android_back_surrenders_connect_field_focus_before_closing() {
        let dir = std::env::temp_dir().join(format!("bone-android-back-{}", std::process::id()));
        let ctx = egui::Context::default();
        let area: VisibleArea = Box::new(|| [0, 0, 400, 800]);
        let mut launcher = Launcher::new(&ctx, dir.clone(), Some(area));
        let _ = connect_frame(&mut launcher, &ctx, Vec::new());
        ctx.memory_mut(|memory| memory.request_focus(egui::Id::new("android-destination")));
        let _ = connect_frame(&mut launcher, &ctx, Vec::new());
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(egui::Id::new("android-destination"))
        );

        let _ = connect_frame(&mut launcher, &ctx, vec![key_event(egui::Key::BrowserBack)]);
        assert!(launcher.app.is_none());
        assert!(ctx.memory(|memory| memory.focused()).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn ime_replay_requires_the_same_focused_editor() {
        let dir = std::env::temp_dir().join(format!("bone-android-ime-{}", std::process::id()));
        let ctx = egui::Context::default();
        let mut launcher = Launcher::new(&ctx, dir.clone(), None);
        let ime = egui::output::IMEOutput {
            purpose: egui::IMEPurpose::Normal,
            rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(100.0, 30.0)),
            cursor_rect: egui::Rect::from_min_size(egui::pos2(4.0, 4.0), egui::vec2(1.0, 20.0)),
            should_interrupt_composition: true,
        };
        let expected = egui::output::IMEOutput {
            should_interrupt_composition: false,
            ..ime
        };
        let mut frame = |focus: &str, ime: Option<egui::output::IMEOutput>| {
            let mut synced = None;
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                ui.memory_mut(|memory| memory.request_focus(egui::Id::new(focus)));
                ui.output_mut(|output| output.ime = ime);
                launcher.sync_ime(ui.ctx());
                synced = ui.ctx().output(|output| output.ime);
            });
            output.textures_delta.clear();
            synced
        };

        frame("ime-field", Some(ime));
        assert_eq!(frame("ime-field", None), Some(expected));
        assert_eq!(frame("other-field", None), None);
        assert!(launcher.last_ime.is_none() && launcher.last_ime_focus.is_none());
        let _ = std::fs::remove_dir_all(dir);
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
        let top = text_top(&output, "Connect to your computer").expect("connect screen");
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
    #[test]
    fn workspace_state_persists_between_launchers() {
        let dir = std::env::temp_dir().join(format!(
            "bone-android-workspace-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let first_ctx = egui::Context::default();
        let mut first = Launcher::new(&first_ctx, dir.clone(), None);
        let expected = WorkspaceState {
            chats: vec![Some(17), None, Some(23)],
            active_chat: 2,
            sidebar_open: Some(false),
        };
        first.persist_workspace(expected.clone());

        let second_ctx = egui::Context::default();
        let second = Launcher::new(&second_ctx, dir.clone(), None);
        assert_eq!(second.workspace, expected);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn workspace_write_replaces_file_and_keeps_state_on_failure() {
        let dir = std::env::temp_dir().join(format!(
            "bone-android-workspace-atomic-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let ctx = egui::Context::default();
        let mut launcher = Launcher::new(&ctx, dir.clone(), None);
        let path = dir.join(WORKSPACE_FILE);
        let temporary = dir.join(format!("{WORKSPACE_FILE}.tmp"));
        let first = WorkspaceState {
            chats: vec![Some(7)],
            active_chat: 0,
            sidebar_open: Some(true),
        };
        launcher.persist_workspace(first.clone());

        assert_eq!(
            std::fs::read(&path).unwrap(),
            serde_json::to_vec(&first).unwrap()
        );
        assert!(!temporary.exists());

        std::fs::create_dir(&temporary).unwrap();
        let second = WorkspaceState {
            chats: vec![Some(8)],
            active_chat: 0,
            sidebar_open: Some(false),
        };
        launcher.persist_workspace(second);

        assert_eq!(launcher.workspace, first);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            serde_json::to_vec(&first).unwrap()
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}

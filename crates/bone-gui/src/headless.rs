//! Drive the GUI without a window: the same [`Gui`] and the same egui
//! drawing code, laid out into an in-memory frame of a given size. Used by
//! the multi-client and conformance tests.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use bone_proto::Connection;
use eframe::egui;

use crate::{Gui, GuiOptions};

pub struct Headless {
    pub gui: Gui,
    /// What was last copied to the clipboard.
    pub copied: Option<String>,
    /// The text screen readers got last frame, after
    /// [`Headless::enable_accesskit`].
    pub accessible: Vec<String>,
    /// The same nodes' roles beside their labels, and the one with the
    /// focus.
    pub accessible_roles: Vec<(String, String)>,
    pub accessible_focus: Option<String>,
    /// When the last frame asked for the next one (`Duration::MAX`: only
    /// on input).
    pub repaint: Duration,
    /// The text last frame painted, each piece with the rectangle it
    /// covers, in painting order.
    pub painted: Vec<(String, egui::Rect)>,
    /// The filled rectangles it painted (boxes, bubbles, bars).
    pub boxes: Vec<egui::Rect>,
    /// Those with the color each was filled with (red, green, blue, alpha).
    pub fills: Vec<(egui::Rect, [u8; 4])>,
    ctx: egui::Context,
    size: egui::Vec2,
    /// Input for the next frame, as the window would send it.
    events: Vec<egui::Event>,
    dropped: Vec<egui::DroppedFileHandle>,
    /// The display's physical pixels per logical point.
    scale: f32,
    /// Whether the window has the focus. Gaining it checks the Lua
    /// config for a saved edit.
    focused: bool,
    /// Seconds spent in [`Gui::ui`] last [`Headless::frame`], not in
    /// gathering the painted shapes afterwards.
    pub ui_secs: f64,
    /// Seconds spent tessellating that frame's shapes, as the window would
    /// before handing them to the GPU.
    pub tess_secs: f64,
}

impl Headless {
    pub async fn start(
        conn: Connection,
        opts: GuiOptions,
        width: f32,
        height: f32,
    ) -> Result<Self, String> {
        Ok(Headless {
            gui: Gui::connect(conn, opts).await?,
            copied: None,
            repaint: Duration::MAX,
            painted: Vec::new(),
            boxes: Vec::new(),
            fills: Vec::new(),
            accessible: Vec::new(),
            accessible_roles: Vec::new(),
            accessible_focus: None,
            ctx: egui::Context::default(),
            size: egui::vec2(width, height),
            events: Vec::new(),
            dropped: Vec::new(),
            scale: 1.0,
            focused: true,
            ui_secs: 0.0,
            tess_secs: 0.0,
        })
    }

    /// Lay out one frame, as the window would draw it.
    pub fn frame(&mut self) {
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, self.size)),
            events: std::mem::take(&mut self.events),
            dropped_files: std::mem::take(&mut self.dropped),
            ..Default::default()
        };
        let viewport = input.viewports.entry(egui::ViewportId::ROOT).or_default();
        viewport.native_pixels_per_point = Some(self.scale);
        viewport.focused = Some(self.focused);
        let gui = &mut self.gui;
        let started = std::time::Instant::now();
        let mut out = self.ctx.run_ui(input, |ui| gui.ui(ui));
        self.ui_secs = started.elapsed().as_secs_f64();
        // No renderer takes the font atlas and images.
        out.textures_delta.clear();
        let shapes = std::mem::take(&mut out.shapes);
        self.painted.clear();
        self.boxes.clear();
        self.fills.clear();
        for clipped in &shapes {
            collect(
                &clipped.shape,
                &mut self.painted,
                &mut self.boxes,
                &mut self.fills,
            );
        }
        let tessellated = std::time::Instant::now();
        let _primitives = self.ctx.tessellate(shapes, self.scale);
        self.tess_secs = tessellated.elapsed().as_secs_f64();
        if let Some(update) = out.platform_output.accesskit_update.take() {
            self.accessible = (update.nodes.iter())
                .filter_map(|(_, n)| n.label().or(n.value()).map(str::to_owned))
                .collect();
            self.accessible_roles = (update.nodes.iter())
                .filter_map(|(_, n)| {
                    let label = n.label().or(n.value())?;
                    Some((format!("{:?}", n.role()), label.to_owned()))
                })
                .collect();
            self.accessible_focus = (update.nodes.iter())
                .find(|(id, _)| *id == update.focus)
                .and_then(|(_, n)| n.label().map(str::to_owned));
        }
        self.repaint = (out.viewport_output.get(&egui::ViewportId::ROOT))
            .map_or(Duration::MAX, |v| v.repaint_delay);
        for cmd in out.platform_output.commands {
            if let egui::OutputCommand::CopyText(text) = cmd {
                self.copied = Some(text);
            }
        }
    }

    /// Handle replies and server events until nothing has happened for
    /// `quiet`, then draw a frame.
    pub async fn settle(&mut self, quiet: Duration) {
        while let Ok(true) = tokio::time::timeout(quiet, self.gui.next()).await {}
        self.frame();
    }

    /// Keep handling events until `pred` holds for the GUI, or fail with the
    /// transcript after `timeout`.
    pub async fn wait_for(
        &mut self,
        timeout: Duration,
        pred: impl Fn(&Gui) -> bool,
    ) -> Result<(), String> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            self.frame();
            if pred(&self.gui) {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(self.gui.transcript());
            }
            self.settle(Duration::from_millis(20)).await;
        }
    }

    /// The window's size in logical points, from the next frame.
    pub fn resize(&mut self, width: f32, height: f32) {
        self.size = egui::vec2(width, height);
    }

    /// The display's scale (2.0 for a HiDPI screen), from the next frame.
    pub fn set_scale(&mut self, scale: f32) {
        self.scale = scale;
    }

    /// Whether the window has the focus, from the next frame. Gaining it
    /// reloads Lua when the config on disk changed.
    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    /// Press `key` with `modifiers` in the next frame.
    pub fn key(&mut self, key: egui::Key, modifiers: egui::Modifiers) {
        for pressed in [true, false] {
            self.events.push(egui::Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers,
            });
        }
    }

    /// Press and release the left button over character cell `(x, y)`
    /// from the window's top left (the cell measured last frame) in the
    /// next frame.
    pub fn click(&mut self, x: u16, y: u16) {
        let (col, row) = self.gui.view.cell.expect("a frame was drawn");
        let pos = egui::pos2((f32::from(x) + 0.5) * col, (f32::from(y) + 0.5) * row);
        self.events.push(egui::Event::PointerMoved(pos));
        for pressed in [true, false] {
            self.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
        }
    }

    /// Move the pointer to logical pixel `(x, y)`.
    pub fn hover_at(&mut self, x: f32, y: f32) {
        self.events
            .push(egui::Event::PointerMoved(egui::pos2(x, y)));
    }

    /// Press (or release) the left button at logical pixel `(x, y)`.
    pub fn button_at(&mut self, x: f32, y: f32, pressed: bool) {
        let pos = egui::pos2(x, y);
        self.events.push(egui::Event::PointerMoved(pos));
        self.events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }

    /// Press and release the left button at logical pixel `(x, y)`.
    pub fn click_at(&mut self, x: f32, y: f32) {
        let pos = egui::pos2(x, y);
        self.events.push(egui::Event::PointerMoved(pos));
        for pressed in [true, false] {
            self.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
        }
    }

    /// Build the accessibility tree from the next frame, as a screen reader
    /// asking for it does.
    pub fn enable_accesskit(&mut self) {
        self.ctx.enable_accesskit();
    }

    /// Drop file `path` on the window in the next frame.
    pub fn drop_file(&mut self, path: PathBuf) {
        self.dropped.push(Arc::new(Dropped(path)));
    }

    /// Paste `text` in the next frame.
    pub fn paste(&mut self, text: &str) {
        self.events.push(egui::Event::Paste(text.to_owned()));
    }

    /// Type `text` in the next frame.
    pub fn text(&mut self, text: &str) {
        self.events.push(egui::Event::Text(text.to_owned()));
    }

    /// Scroll the wheel by `dx`/`dy` points in the next frame.
    pub fn wheel(&mut self, dx: f32, dy: f32) {
        self.events.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(dx, dy),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        });
    }

    /// Type `text` into the prompt and send it, as Enter does.
    pub fn send(&mut self, text: &str) {
        self.gui.draft = text.to_owned();
        self.gui.submit();
    }
}

fn collect(
    shape: &egui::Shape,
    text: &mut Vec<(String, egui::Rect)>,
    boxes: &mut Vec<egui::Rect>,
    fills: &mut Vec<(egui::Rect, [u8; 4])>,
) {
    match shape {
        egui::Shape::Text(t) => {
            text.push((
                t.galley.text().to_owned(),
                t.galley.rect.translate(t.pos.to_vec2()),
            ));
        }
        egui::Shape::Rect(r) if r.fill.a() > 0 => {
            boxes.push(r.rect);
            fills.push((r.rect, r.fill.to_srgba_unmultiplied()));
        }
        egui::Shape::Vec(v) => {
            for s in v {
                collect(s, text, boxes, fills);
            }
        }
        _ => {}
    }
}

/// A file dropped on the window.
#[derive(Debug)]
struct Dropped(PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

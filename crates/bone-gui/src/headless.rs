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
    /// When the last frame asked for the next one (`Duration::MAX`: only
    /// on input).
    pub repaint: Duration,
    ctx: egui::Context,
    size: egui::Vec2,
    /// Input for the next frame, as the window would send it.
    events: Vec<egui::Event>,
    dropped: Vec<egui::DroppedFileHandle>,
    /// The display's physical pixels per logical point.
    scale: f32,
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
            ctx: egui::Context::default(),
            size: egui::vec2(width, height),
            events: Vec::new(),
            dropped: Vec::new(),
            scale: 1.0,
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
        input
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .native_pixels_per_point = Some(self.scale);
        let gui = &mut self.gui;
        let mut out = self.ctx.run_ui(input, |ui| gui.ui(ui));
        // No renderer takes the font atlas and images.
        out.textures_delta.clear();
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

    /// Press and release the left button over character cell `(x, y)` (as
    /// laid out last frame) in the next frame.
    pub fn click(&mut self, x: u16, y: u16) {
        let (ox, oy, col, row) = self.gui.lua.cells.expect("a frame was drawn");
        let pos = egui::pos2(
            ox + (f32::from(x) + 0.5) * col,
            oy + (f32::from(y) + 0.5) * row,
        );
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

    /// Drop file `path` on the window in the next frame.
    pub fn drop_file(&mut self, path: PathBuf) {
        self.dropped.push(Arc::new(Dropped(path)));
    }

    /// Type `text` in the next frame.
    pub fn text(&mut self, text: &str) {
        self.events.push(egui::Event::Text(text.to_owned()));
    }

    /// Type `text` into the prompt and send it, as Enter does.
    pub fn send(&mut self, text: &str) {
        self.gui.draft = text.to_owned();
        self.gui.submit();
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

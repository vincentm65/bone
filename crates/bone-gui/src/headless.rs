//! Drive the GUI without a window: the same [`Gui`] and the same egui
//! drawing code, laid out into an in-memory frame of a given size. Used by
//! the multi-client and conformance tests.

use std::time::Duration;

use bone_proto::Connection;
use eframe::egui;

use crate::{Gui, GuiOptions};

pub struct Headless {
    pub gui: Gui,
    ctx: egui::Context,
    size: egui::Vec2,
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
            ctx: egui::Context::default(),
            size: egui::vec2(width, height),
        })
    }

    /// Lay out one frame, as the window would draw it.
    pub fn frame(&mut self) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, self.size)),
            ..Default::default()
        };
        let gui = &mut self.gui;
        let mut out = self.ctx.run_ui(input, |ui| gui.ui(ui));
        // No renderer takes the font atlas and images.
        out.textures_delta.clear();
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

    /// Type `text` into the prompt and send it, as Enter does.
    pub fn send(&mut self, text: &str) {
        self.gui.draft = text.to_owned();
        self.gui.submit();
    }
}

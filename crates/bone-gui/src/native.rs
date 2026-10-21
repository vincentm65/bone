//! The window. Drawn with wgpu, preferring the integrated GPU (low power)
//! unless `WGPU_POWER_PREF` says otherwise, and with OpenGL (glow) if wgpu
//! cannot start; the renderer in use is shown in the status line.

use std::cell::Cell;

use bone_proto::Connection;
use eframe::egui;
use eframe::egui_wgpu::WgpuSetup;
use eframe::wgpu::PowerPreference;

use crate::{Gui, GuiOptions};

struct App {
    gui: Gui,
    title: String,
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.gui.ui(ui);
        let title = format!("{} — bone", self.gui.title());
        if title != self.title {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
        if self.gui.closed {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

fn options(renderer: eframe::Renderer) -> eframe::NativeOptions {
    let mut opts = eframe::NativeOptions {
        renderer,
        viewport: egui::ViewportBuilder::default()
            .with_title("bone")
            .with_inner_size([960.0, 720.0]),
        ..Default::default()
    };
    if let WgpuSetup::CreateNew(setup) = &mut opts.wgpu_options.wgpu_setup {
        setup.power_preference = PowerPreference::from_env().unwrap_or(PowerPreference::LowPower);
    }
    opts
}

/// Open the GUI on `conn` and run it until the window closes. Call from
/// the main thread, inside a multi-threaded Tokio runtime.
pub async fn run(conn: Connection, opts: GuiOptions) -> Result<(), String> {
    let gui = Gui::connect(conn, opts).await?;
    tokio::task::block_in_place(move || {
        let slot = Cell::new(Some(gui));
        let mut failed = Vec::new();
        for (renderer, name) in [
            (eframe::Renderer::Wgpu, "wgpu"),
            (eframe::Renderer::Glow, "OpenGL"),
        ] {
            let started = Cell::new(false);
            let r = eframe::run_native(
                "bone",
                options(renderer),
                Box::new(|_cc| {
                    started.set(true);
                    let mut gui = slot.take().expect("the GUI starts once");
                    gui.info(format!("drawing with {name}"));
                    Ok(Box::new(App {
                        gui,
                        title: String::new(),
                    }))
                }),
            );
            match r {
                Ok(()) => return Ok(()),
                // Past window creation there is no GUI left to retry with.
                Err(e) if started.get() => return Err(e.to_string()),
                Err(e) => failed.push(format!("{name}: {e}")),
            }
        }
        Err(format!("cannot open a window ({})", failed.join("; ")))
    })
}

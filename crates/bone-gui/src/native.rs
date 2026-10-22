//! The window. Drawn with wgpu, preferring the integrated GPU (low power)
//! unless `WGPU_POWER_PREF` says otherwise, and with OpenGL (glow) if wgpu
//! cannot start; the renderer in use (and a software one) is shown in the
//! status line. If the GPU device is lost (a driver reset, an unplugged
//! GPU), the window closes and opens again on a new one, with the same
//! session and Lua state.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use bone_proto::Connection;
use eframe::egui;
use eframe::egui_wgpu::WgpuSetup;
use eframe::wgpu::PowerPreference;

use crate::{Gui, GuiOptions};

struct App {
    /// `None` once the device was lost and the GUI went back to `run`.
    gui: Option<Gui>,
    title: String,
    lost: Arc<AtomicBool>,
    /// Where the GUI goes when the device is lost.
    slot: Rc<RefCell<Option<Gui>>>,
}

impl eframe::App for App {
    /// Every plugin's shutdown hooks, and its saved state, before the
    /// window goes (not when it is only opening again).
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(gui) = &mut self.gui {
            bone_ui::lua::plugins::run_shutdown(gui, None);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.lost.load(Ordering::Relaxed) {
            if let Some(gui) = self.gui.take() {
                *self.slot.borrow_mut() = Some(gui);
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
            return;
        }
        let Some(gui) = &mut self.gui else { return };
        gui.ui(ui);
        let title = (gui.window_title.clone()).unwrap_or_else(|| format!("{} — bone", gui.title()));
        if title != self.title {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
        if gui.closed {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

/// Set `lost` when `device` is lost, and ask for a frame to act on it.
/// Errors after that are from drawing on the lost device and are ignored;
/// others are fatal, as they are by default.
fn watch_device(device: &eframe::wgpu::Device, lost: &Arc<AtomicBool>, ctx: &egui::Context) {
    let (flag, ctx) = (lost.clone(), ctx.clone());
    device.set_device_lost_callback(move |reason, message| {
        if reason == eframe::wgpu::DeviceLostReason::Unknown {
            eprintln!("bone: the GPU device was lost ({message}); opening the window again");
            flag.store(true, Ordering::Relaxed);
            ctx.request_repaint();
        }
    });
    let flag = lost.clone();
    device.on_uncaptured_error(Arc::new(move |e| {
        if !flag.load(Ordering::Relaxed) {
            panic!("wgpu error: {e}");
        }
    }));
}

fn options(renderer: eframe::Renderer, size: [f32; 2]) -> eframe::NativeOptions {
    let mut opts = eframe::NativeOptions {
        renderer,
        viewport: egui::ViewportBuilder::default()
            .with_title("bone")
            .with_inner_size(size),
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
        let slot = Rc::new(RefCell::new(Some(gui)));
        // A window that loses its device this many times stays closed.
        for _ in 0..3 {
            open(&slot)?;
            if slot.borrow().is_none() {
                return Ok(());
            }
        }
        Err("the GPU device was lost again; giving up".into())
    })
}

/// Run the GUI in `slot` in a window until it closes, or until the device
/// is lost, which puts it back in `slot`.
fn open(slot: &Rc<RefCell<Option<Gui>>>) -> Result<(), String> {
    // Opened again, the window keeps its size.
    let size = slot.borrow().as_ref().and_then(|g| {
        let (w, h) = (
            g.window_info["width"].as_f64()?,
            g.window_info["height"].as_f64()?,
        );
        (w > 0.0 && h > 0.0).then_some([w as f32, h as f32])
    });
    let mut failed = Vec::new();
    for (renderer, name) in [
        (eframe::Renderer::Wgpu, "wgpu"),
        (eframe::Renderer::Glow, "OpenGL"),
    ] {
        let started = Cell::new(false);
        let r = eframe::run_native(
            "bone",
            options(renderer, size.unwrap_or([960.0, 720.0])),
            Box::new(|cc| {
                started.set(true);
                let mut gui = slot.borrow_mut().take().expect("a GUI to open");
                let lost = Arc::new(AtomicBool::new(false));
                let (renderer, software) = match &cc.wgpu_render_state {
                    Some(rs) => {
                        watch_device(&rs.device, &lost, &cc.egui_ctx);
                        let i = rs.adapter.get_info();
                        let software = i.device_type == eframe::wgpu::DeviceType::Cpu;
                        (format!("{name} on {} ({:?})", i.name, i.backend), software)
                    }
                    None => (name.to_owned(), false),
                };
                if software {
                    gui.error(format!(
                        "no GPU in use: drawing in software with {renderer}"
                    ));
                } else {
                    gui.info(format!("drawing with {renderer}"));
                }
                gui.renderer = Some(renderer);
                // A window opened again is drawn with new fonts.
                gui.lua.fonts = None;
                Ok(Box::new(App {
                    gui: Some(gui),
                    title: String::new(),
                    lost,
                    slot: slot.clone(),
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
}

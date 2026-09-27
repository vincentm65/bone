//! Bone's phone client: a thin frontend of a Bone daemon on another machine,
//! reached over SSH (`ssh <host> -- bone stdio`).
//!
//! Isolated from the desktop app (`native/`): it shares only `protocol` (the
//! messages) and `client` (the transport), and keeps its own small reducer and
//! touch UI. The same code runs as a phone-sized desktop window
//! (`bone-android-preview`) for development without a device.

pub mod app;
pub mod link;
pub mod state;

pub use app::PhoneApp;
pub use link::Target;

/// Run the app in a phone-sized desktop window.
pub fn run_preview(target: Option<Target>) -> eframe::Result {
    eframe::run_native(
        "Bone (phone preview)",
        eframe::NativeOptions {
            viewport: eframe::egui::ViewportBuilder::default()
                .with_inner_size([400.0, 820.0])
                .with_min_inner_size([320.0, 480.0]),
            ..Default::default()
        },
        Box::new(move |cc| Ok(Box::new(PhoneApp::new(&cc.egui_ctx, target)))),
    )
}

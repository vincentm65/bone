//! Bone's phone client: a thin frontend of a Bone daemon on another machine,
//! reached over in-app SSH running `bone stdio` there.
//!
//! Isolated from the desktop app (`native/`): it shares only `protocol` (the
//! messages) and `client` (the transport), and keeps its own small reducer and
//! touch UI. The same code runs as a phone-sized desktop window
//! (`bone-android-preview`) for development without a device.

pub mod app;
pub mod link;
pub mod ssh;
pub mod state;

pub use app::PhoneApp;
pub use link::Target;

/// Run the app in a phone-sized desktop window.
pub fn run_preview(target: Option<Target>, data_dir: std::path::PathBuf) -> eframe::Result {
    eframe::run_native(
        "Bone (phone preview)",
        eframe::NativeOptions {
            viewport: eframe::egui::ViewportBuilder::default()
                .with_inner_size([400.0, 820.0])
                .with_min_inner_size([320.0, 480.0]),
            ..Default::default()
        },
        Box::new(move |cc| Ok(Box::new(PhoneApp::new(&cc.egui_ctx, target, data_dir)))),
    )
}

/// Android entry point, called by the NativeActivity glue.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: winit::platform::android::activity::AndroidApp) {
    let data_dir = app
        .internal_data_path()
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let result = eframe::run_native(
        "Bone",
        eframe::NativeOptions {
            android_app: Some(app),
            ..Default::default()
        },
        Box::new(move |cc| Ok(Box::new(PhoneApp::new(&cc.egui_ctx, None, data_dir)))),
    );
    if let Err(error) = result {
        eprintln!("bone-android: {error}");
    }
}

//! Renderer bootstrap. Retry only before app creation, in a fresh process:
//! winit event loops cannot safely be recreated after a failed initialization.
use std::{
    cmp::Reverse,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RendererChoice {
    Auto,
    Wgpu,
    Glow,
}

impl RendererChoice {
    fn parse(value: Option<&str>) -> Result<Self, &'static str> {
        match value {
            None | Some("auto") => Ok(Self::Auto),
            Some("wgpu") => Ok(Self::Wgpu),
            Some("glow") => Ok(Self::Glow),
            _ => Err("BONE_DESKTOP_RENDERER must be auto, wgpu, or glow"),
        }
    }

    fn renderer(self) -> eframe::Renderer {
        match self {
            Self::Auto | Self::Wgpu => eframe::Renderer::Wgpu,
            Self::Glow => eframe::Renderer::Glow,
        }
    }

    fn should_retry(self, app_started: bool, error: &eframe::Error) -> bool {
        self == Self::Auto && !app_started && matches!(error, eframe::Error::Wgpu(_))
    }
}

fn adapter_name_filter() -> Result<Option<String>, String> {
    match std::env::var_os("WGPU_ADAPTER_NAME") {
        None => Ok(None),
        Some(value) => value
            .into_string()
            .map(|value| {
                let value = value.trim().to_owned();
                (!value.is_empty()).then_some(value)
            })
            .map_err(|_| "WGPU_ADAPTER_NAME must be valid Unicode".to_owned()),
    }
}

fn adapter_name_matches(adapter_name: &str, requested: Option<&str>) -> bool {
    requested.is_none_or(|requested| {
        adapter_name
            .to_ascii_lowercase()
            .contains(&requested.to_ascii_lowercase())
    })
}

fn adapter_rank(device_type: eframe::egui_wgpu::wgpu::DeviceType) -> u8 {
    use eframe::egui_wgpu::wgpu::DeviceType;

    match device_type {
        DeviceType::DiscreteGpu => 4,
        DeviceType::IntegratedGpu => 3,
        DeviceType::VirtualGpu => 2,
        DeviceType::Other => 1,
        DeviceType::Cpu => 0,
    }
}

fn has_presentable_surface_formats(formats: &[eframe::egui_wgpu::wgpu::TextureFormat]) -> bool {
    !formats.is_empty()
}

fn select_adapter(
    adapters: &[eframe::egui_wgpu::wgpu::Adapter],
    surface: Option<&eframe::egui_wgpu::wgpu::Surface<'_>>,
    device_descriptor: &dyn Fn(
        &eframe::egui_wgpu::wgpu::Adapter,
    ) -> eframe::egui_wgpu::wgpu::DeviceDescriptor<'static>,
) -> Result<eframe::egui_wgpu::wgpu::Adapter, String> {
    let requested_name = adapter_name_filter()?;
    let mut candidates: Vec<_> = adapters
        .iter()
        .filter(|adapter| adapter_name_matches(&adapter.get_info().name, requested_name.as_deref()))
        .filter(|adapter| {
            surface.is_none_or(|surface| {
                has_presentable_surface_formats(&surface.get_capabilities(adapter).formats)
            })
        })
        .collect();
    candidates.sort_by_key(|adapter| Reverse(adapter_rank(adapter.get_info().device_type)));

    if candidates.is_empty() {
        return Err(match requested_name {
            Some(name) => format!("no presentable wgpu adapter matched WGPU_ADAPTER_NAME={name:?}"),
            None => "no presentable wgpu adapter was found".to_owned(),
        });
    }

    let mut failures = Vec::new();
    for adapter in candidates {
        let info = adapter.get_info();
        if pollster::block_on(adapter.request_device(&device_descriptor(adapter))).is_ok() {
            return Ok(adapter.clone());
        }
        failures.push(format!("{} ({:?})", info.name, info.device_type));
    }

    Err(format!(
        "presentable wgpu adapters could not create an egui device: {}",
        failures.join(", ")
    ))
}

fn wgpu_configuration() -> eframe::WgpuConfiguration {
    let mut setup = eframe::egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    let device_descriptor = Arc::clone(&setup.device_descriptor);
    setup.native_adapter_selector = Some(Arc::new(move |adapters, surface| {
        select_adapter(adapters, surface, &*device_descriptor)
    }));
    eframe::WgpuConfiguration {
        wgpu_setup: setup.into(),
        ..Default::default()
    }
}

pub fn run(cli: crate::cli::Cli) -> eframe::Result {
    let value = std::env::var("BONE_DESKTOP_RENDERER");
    let choice = RendererChoice::parse(match &value {
        Ok(value) => Some(value.as_str()),
        Err(std::env::VarError::NotPresent) => None,
        Err(_) => Some("invalid"),
    })
    .unwrap_or_else(|message| {
        eprintln!("{message}");
        std::process::exit(2);
    });
    let started = Arc::new(AtomicBool::new(false));
    let app_started = started.clone();
    let result = eframe::run_native(
        "Bone Desktop",
        eframe::NativeOptions {
            renderer: choice.renderer(),
            wgpu_options: wgpu_configuration(),
            viewport: eframe::egui::ViewportBuilder::default()
                .with_inner_size([1000.0, 720.0])
                .with_min_inner_size([520.0, 400.0]),
            ..Default::default()
        },
        Box::new(move |cc| {
            app_started.store(true, Ordering::Relaxed);
            Ok(Box::new(crate::DesktopApp::new(cc.egui_ctx.clone(), cli)))
        }),
    );
    if let Err(error) = &result {
        if choice.should_retry(started.load(Ordering::Relaxed), error) {
            eprintln!(
                "Bone Desktop: GPU initialization failed: {error}\nRetrying once with OpenGL (glow). Set BONE_DESKTOP_RENDERER=wgpu to disable fallback."
            );
            if let Err(retry_error) = restart_with_glow() {
                eprintln!("Bone Desktop: could not start the OpenGL fallback: {retry_error}");
            }
        } else {
            eprintln!(
                "Bone Desktop could not run: {error}\nTry BONE_DESKTOP_RENDERER=glow or BONE_DESKTOP_RENDERER=wgpu; WGPU_ADAPTER_NAME optionally filters wgpu adapters by case-insensitive name substring."
            );
        }
    }
    result
}

fn restart_with_glow() -> std::io::Result<()> {
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command
        .args(std::env::args_os().skip(1))
        .env("BONE_DESKTOP_RENDERER", "glow");
    // Replace the failed process on Unix: preserve its PID, signals, working
    // directory, arguments and environment without leaving a waiting parent.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Err(command.exec())
    }
    #[cfg(not(unix))]
    {
        let status = command.status()?;
        std::process::exit(status.code().unwrap_or(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renderer_overrides_are_explicit_and_validated() {
        assert_eq!(RendererChoice::parse(None), Ok(RendererChoice::Auto));
        assert_eq!(
            RendererChoice::parse(Some("auto")),
            Ok(RendererChoice::Auto)
        );
        assert_eq!(
            RendererChoice::parse(Some("wgpu")),
            Ok(RendererChoice::Wgpu)
        );
        assert_eq!(
            RendererChoice::parse(Some("glow")),
            Ok(RendererChoice::Glow)
        );
        assert!(RendererChoice::parse(Some("gl")).is_err());
    }

    #[test]
    fn adapter_selection_helpers_prefer_real_presentable_gpus() {
        use eframe::egui_wgpu::wgpu::{DeviceType, TextureFormat};

        assert!(adapter_rank(DeviceType::DiscreteGpu) > adapter_rank(DeviceType::IntegratedGpu));
        assert!(adapter_rank(DeviceType::IntegratedGpu) > adapter_rank(DeviceType::Cpu));
        assert!(adapter_rank(DeviceType::VirtualGpu) > adapter_rank(DeviceType::Other));
        assert!(has_presentable_surface_formats(&[
            TextureFormat::Bgra8Unorm
        ]));
        assert!(!has_presentable_surface_formats(&[]));
    }

    #[test]
    fn adapter_name_filter_is_case_insensitive_and_optional() {
        assert!(adapter_name_matches("AMD Radeon PRO", Some("radeon")));
        assert!(adapter_name_matches("AMD Radeon PRO", Some("PRO")));
        assert!(!adapter_name_matches("AMD Radeon PRO", Some("intel")));
        assert!(adapter_name_matches("anything", None));
    }

    #[test]
    fn fallback_is_only_for_automatic_pre_app_gpu_failure() {
        let gpu_error =
            eframe::Error::Wgpu(eframe::egui_wgpu::WgpuError::NoSurfaceFormatsAvailable);
        assert!(RendererChoice::Auto.should_retry(false, &gpu_error));
        assert!(!RendererChoice::Auto.should_retry(true, &gpu_error));
        assert!(!RendererChoice::Wgpu.should_retry(false, &gpu_error));
        assert!(!RendererChoice::Glow.should_retry(false, &gpu_error));
        let app_error = eframe::Error::AppCreation(std::io::Error::other("test").into());
        assert!(!RendererChoice::Auto.should_retry(false, &app_error));
    }
}

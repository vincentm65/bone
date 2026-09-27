pub use bone_render::theme::*;

/// Load the configured application theme directly from canonical settings.
/// A missing settings file has no configured overrides and uses defaults;
/// malformed settings remain an error so entry points can warn explicitly.
pub fn load_configured() -> Result<Theme, crate::config::settings::SettingsError> {
    let Some(settings) = crate::config::settings::Settings::load()? else {
        return Ok(Theme::default());
    };
    Ok(Theme::from_snapshot(&settings.resolved().theme))
}

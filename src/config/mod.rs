mod app_config;
mod paths;
mod providers_config;
mod seed;

use std::path::Path;

#[allow(unused_imports)]
pub use app_config::UserConfig;
pub use app_config::load_user_config;
#[allow(unused_imports)]
pub use paths::{config_path, providers_path};
pub use providers_config::{ProviderEntry, ProvidersConfig, load_providers};
pub use seed::seed_providers_if_missing;

/// Shared YAML loader used by both config modules.
pub(crate) fn load_yaml<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    let raw = std::fs::read_to_string(path).ok()?;
    // Strip BOM if present
    let raw = raw.trim_start_matches('\u{feff}');
    serde_yaml::from_str(raw).ok()
}

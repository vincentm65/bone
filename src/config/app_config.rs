use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
pub struct UserConfig {
    /// Active provider id — must match a key in providers.yaml.
    #[serde(default = "default_provider")]
    pub provider: String,
}

fn default_provider() -> String {
    "local".to_string()
}

impl Default for UserConfig {
    fn default() -> Self {
        Self {
            provider: default_provider(),
        }
    }
}

pub fn load_user_config() -> UserConfig {
    let path = super::paths::config_path();
    if !path.exists() {
        return UserConfig::default();
    }
    super::load_yaml(&path).unwrap_or_else(|| {
        eprintln!("bone: warning: failed to parse {}", path.display());
        UserConfig::default()
    })
}

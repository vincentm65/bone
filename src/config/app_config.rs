use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
pub struct UserConfig {
    /// Active provider id — must match a key in providers.yaml.
    #[serde(default = "default_provider")]
    pub provider: String,

    /// Context window size in tokens.
    #[serde(default = "default_context_window")]
    pub context_window: usize,

    /// Tokens reserved for the model's response.
    #[serde(default = "default_response_budget")]
    pub response_budget: usize,
}

fn default_provider() -> String {
    "local".to_string()
}
fn default_context_window() -> usize {
    200_000
}
fn default_response_budget() -> usize {
    64_000
}

impl Default for UserConfig {
    fn default() -> Self {
        Self {
            provider: default_provider(),
            context_window: default_context_window(),
            response_budget: default_response_budget(),
        }
    }
}

pub fn load_user_config() -> UserConfig {
    let path = super::paths::config_path();
    if !path.exists() {
        return UserConfig::default();
    }
    super::load_yaml(&path).unwrap_or_default()
}

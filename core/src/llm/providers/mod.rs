//! Provider factory: constructs a provider by id from a loaded config.

use super::provider::{LlmError, LlmErrorKind, LlmProvider};
use crate::config::ProvidersConfig;

pub mod codex;
pub mod openai_compat;

/// Construct a provider by stable id, set its model, and return it.
pub fn build_provider(
    id: &str,
    model: &str,
    config: &ProvidersConfig,
) -> Result<Box<dyn LlmProvider>, LlmError> {
    let mut p = create_provider_with_config(id, config)?;
    if !model.is_empty() && p.model() != model {
        p.set_model(model.to_string());
    }
    Ok(p)
}

/// Construct a provider by stable id, using a pre-loaded config.
pub fn create_provider_with_config(
    id: &str,
    config: &ProvidersConfig,
) -> Result<Box<dyn LlmProvider>, LlmError> {
    if let Some(entry) = config.providers.get(id) {
        match entry.handler.as_str() {
            "codex" => {
                return Ok(Box::new(codex::CodexProvider::from_entry(id, entry)));
            }
            "openai" | "" => {
                return Ok(Box::new(openai_compat::OpenAiCompatProvider::from_entry(
                    id, entry,
                )));
            }
            _ => {
                return Err(LlmError::new_with_kind(
                    LlmErrorKind::Config,
                    format!(
                        "unsupported handler `{}` for provider `{id}`; supported: openai, codex",
                        entry.handler
                    ),
                ));
            }
        }
    }
    let available: Vec<&str> = config.providers.keys().map(|s| s.as_str()).collect();
    let mut msg = format!("unknown provider `{id}`.");
    if available.is_empty() {
        msg.push_str(" No providers configured — create ~/.bone-rust/config/providers.yaml");
    } else {
        msg.push_str(&format!(" Available: {}", available.join(", ")));
    }
    Err(LlmError::new_with_kind(LlmErrorKind::Config, msg))
}

#[cfg(test)]
mod tests {
    use super::create_provider_with_config;
    use crate::config::custom::CustomConfigPage;
    use crate::config::providers_config::{ProviderEntry, ProvidersConfig};

    #[test]
    fn seeded_minimax_providers_use_supported_handler() {
        // Load the single source of truth: the page-format providers.yaml
        // that ships to ~/.bone-rust/config/providers.yaml on first run.
        let page: CustomConfigPage =
            serde_yaml::from_str(include_str!("../../config/pages/providers.yaml")).unwrap();
        let mut config = ProvidersConfig::default();
        for field in &page.fields {
            if matches!(
                field.field_type,
                crate::config::custom::ConfigFieldType::Provider
            ) {
                if let Some(value) = &field.value
                    && let Some(entry) = ProviderEntry::from_nested(value)
                {
                    config.providers.insert(field.key.clone(), entry);
                }
            }
        }

        create_provider_with_config("minimax", &config).unwrap();
        create_provider_with_config("minimax_plan", &config).unwrap();
    }
}

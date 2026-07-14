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
    // The seeded Anthropic entry uses the `anthropic` handler, which the
    // factory must build rather than reject.
    create_provider_with_config("anthropic", &config).unwrap();
    create_provider_with_config("grok_build", &config).unwrap();
}

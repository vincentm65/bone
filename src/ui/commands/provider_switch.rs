use crate::config;
use crate::config::ProvidersConfig;
use crate::llm::LlmProvider;
use crate::llm::providers;

pub async fn run(
    arg: &str,
    llm: &mut Box<dyn LlmProvider>,
    provider_label: &mut String,
    model_label: &mut String,
    providers_config: &mut ProvidersConfig,
) -> String {
    if arg.is_empty() {
        let mut lines = vec![format!("Current: {} ({})", model_label, provider_label)];
        lines.push(String::new());
        if providers_config.providers.is_empty() {
            lines.push("No providers configured. Create ~/.bone-rust/providers.yaml".to_string());
        } else {
            lines.push("Available:".to_string());
            for (id, entry) in &providers_config.providers {
                let marker = if id == llm.id() { " *" } else { "" };
                lines.push(format!(
                    "  {} — {} ({}){}",
                    id, entry.label, entry.model, marker
                ));
            }
        }
        lines.join("\n")
    } else {
        match providers::create_provider_with_config(arg, providers_config) {
            Ok(new_provider) => match new_provider.validate().await {
                Ok(()) => {
                    let label = format!("{} ({})", new_provider.name(), new_provider.id());
                    let model = new_provider.model().to_string();
                    *llm = new_provider;
                    *provider_label = label.clone();
                    *model_label = model.clone();
                    providers_config.last_provider = arg.to_string();
                    config::providers_config::save_providers(providers_config);
                    format!("Switched to {} ({})", model, label)
                }
                Err(err) => format!("Provider validation failed: {err}"),
            },
            Err(err) => err.to_string(),
        }
    }
}

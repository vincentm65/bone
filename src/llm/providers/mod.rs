use super::provider::{LlmError, LlmErrorKind, LlmProvider};
use crate::config::ProvidersConfig;

pub mod openai_compat;

/// Construct a provider by stable id, using a pre-loaded config.
pub fn create_provider_with_config(
    id: &str,
    config: &ProvidersConfig,
) -> Result<Box<dyn LlmProvider>, LlmError> {
    if let Some(entry) = config.providers.get(id) {
        if entry.handler == "openai" || entry.handler.is_empty() {
            return Ok(Box::new(openai_compat::OpenAiCompatProvider::from_entry(id, entry)));
        }
        return Err(LlmError::new_with_kind(
            LlmErrorKind::Config,
            format!(
                "unsupported handler `{}` for provider `{id}`; supported: openai",
                entry.handler
            ),
        ));
    }

    // Helpful error listing what's available.
    let available: Vec<&str> = config.providers.keys().map(|s| s.as_str()).collect();
    let mut msg = format!("unknown provider `{id}`.");
    if available.is_empty() {
        msg.push_str(" No providers configured — create ~/.bone-rust/providers.yaml");
    } else {
        msg.push_str(&format!(" Available: {}", available.join(", ")));
    }
    Err(LlmError::new_with_kind(LlmErrorKind::Config, msg))
}

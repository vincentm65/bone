//! Provider factory: constructs a provider by id from a loaded config.

use super::provider::{LlmError, LlmErrorKind, LlmProvider};
use crate::config::ProvidersConfig;

pub mod anthropic;
pub mod codex;
pub mod grok_build;
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
            "grok_build" => {
                return Ok(Box::new(grok_build::GrokBuildProvider::from_entry(
                    id, entry,
                )));
            }
            "anthropic" => {
                return Ok(Box::new(anthropic::AnthropicProvider::from_entry(
                    id, entry,
                )));
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
                        "unsupported handler `{}` for provider `{id}`; supported: openai, anthropic, codex, grok_build",
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
#[path = "providers_tests.rs"]
mod providers_tests;

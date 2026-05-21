use async_stream::try_stream;
use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures_util::TryStreamExt;
use serde::Serialize;
use serde_json::Value;

use crate::config::ProviderEntry;
use crate::llm::provider::{ChatMessage, LlmError, LlmErrorKind, LlmProvider, ResponseStream};

/// Generic OpenAI-compatible provider.
///
/// Works with any server that implements the `/chat/completions` streaming
/// endpoint (OpenAI format): llama.cpp, OpenRouter, GLM, Gemini, Kimi,
/// DeepSeek, etc.
///
/// The `endpoint` field in `ProviderEntry` controls the path appended to
/// `base_url`.  Default: `/chat/completions`.  For local llama.cpp you
/// typically set `endpoint: /v1/chat/completions`.
pub struct OpenAiCompatProvider {
    client: reqwest::Client,
    base_url: String,
    model: String,
    api_key: String,
    endpoint: String,
    id: String,
    label: String,
}

impl OpenAiCompatProvider {
    pub fn from_entry(id: &str, entry: &ProviderEntry) -> Self {
        let label = if entry.label.is_empty() {
            id.to_string()
        } else {
            entry.label.clone()
        };
        Self {
            client: reqwest::Client::new(),
            id: id.to_string(),
            label,
            base_url: entry.base_url.trim_end_matches('/').to_string(),
            model: entry.model.clone(),
            api_key: entry.api_key.clone(),
            endpoint: entry.endpoint.clone(),
        }
    }

    fn chat_url(&self) -> String {
        format!("{}{}", self.base_url, self.endpoint)
    }
}

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    stream: bool,
}

#[async_trait]
impl LlmProvider for OpenAiCompatProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.label
    }

    fn model(&self) -> &str {
        &self.model
    }

    async fn validate(&self) -> Result<(), LlmError> {
        // For local providers (no API key), hit /health.
        // For remote providers, skip validation — errors will surface on first chat.
        if self.api_key.is_empty() {
            let health_url = format!("{}/health", self.base_url);
            let resp = self.client.get(&health_url).send().await;
            match resp {
                Ok(r) if r.status().is_success() => Ok(()),
                Ok(r) => Err(LlmError::new_with_kind(
                    LlmErrorKind::Server(r.status().as_u16()),
                    format!(
                        "local server returned {} from /health — is llama.cpp running?",
                        r.status()
                    ),
                )),
                Err(e) => Err(LlmError::new_with_kind(
                    LlmErrorKind::Connection,
                    format!(
                        "can't reach {}/health: {e} — is llama.cpp running?",
                        self.base_url
                    ),
                )),
            }
        } else {
            Ok(())
        }
    }

    async fn chat_stream(&self, messages: Vec<ChatMessage>) -> Result<ResponseStream, LlmError> {
        let request = ChatRequest {
            model: self.model.clone(),
            messages,
            stream: true,
        };

        let mut req = self.client.post(self.chat_url()).json(&request);

        if !self.api_key.is_empty() {
            req = req.bearer_auth(&self.api_key);
        }

        let response = req.send().await?;
        let status = response.status();
        if !status.is_success() {
            let kind = match status.as_u16() {
                401 | 403 => LlmErrorKind::Auth,
                429 => LlmErrorKind::RateLimit,
                code if code >= 500 => LlmErrorKind::Server(code),
                _ => LlmErrorKind::Config,
            };
            return Err(LlmError::new_with_kind(
                kind,
                format!("HTTP {} from {}", status, self.chat_url()),
            ));
        }

        let events = response.bytes_stream().eventsource();

        let stream = try_stream! {
            futures_util::pin_mut!(events);

            while let Some(event) = events.try_next().await.map_err(|err| LlmError::new(err.to_string()))? {
                let data = event.data.trim();
                if data.is_empty() || data == "[DONE]" {
                    continue;
                }

                // Skip SSE comments (OpenRouter sends these)
                if data.starts_with(':') {
                    continue;
                }

                let value: Value = serde_json::from_str(data)?;
                if let Some(content) = value
                    .get("choices")
                    .and_then(|choices| choices.get(0))
                    .and_then(|choice| choice.get("delta"))
                    .and_then(|delta| delta.get("content"))
                    .and_then(|content| content.as_str())
                    && !content.is_empty() {
                        yield content.to_string();
                    }
            }
        };

        Ok(Box::pin(stream))
    }
}

//! Subscription-backed Grok Build HTTP provider.
//!
//! Grok Build's subscription path is an OpenAI Chat Completions-compatible
//! endpoint behind the Grok CLI inference proxy. This module keeps the transport
//! separate from the normal API-key providers while handing Bone's tool
//! definitions to the model and returning model tool calls to the runtime.
//! Bone, not the Grok CLI, executes and approves tools.

use async_trait::async_trait;
use base64::Engine;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

use crate::config::ProviderEntry;
use crate::llm::provider::{
    ChatMessage, LlmError, LlmErrorKind, LlmProvider, ProviderRequestContext, ResponseStream,
};
use crate::tools::ToolDefinition;

use super::openai_compat::OpenAiCompatProvider;

const DEFAULT_BASE_URL: &str = "https://cli-chat-proxy.grok.com/v1";
const DEFAULT_ENDPOINT: &str = "/chat/completions";
const DEFAULT_MODEL: &str = "grok-build";
const OIDC_ISSUER: &str = "https://auth.x.ai";
const OIDC_DISCOVERY_PATH: &str = "/.well-known/openid-configuration";
const OAUTH_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const DEFAULT_CLIENT_VERSION: &str = "0.2.22";
const REFRESH_SKEW: Duration = Duration::from_secs(120);

/// Subscription-backed Grok provider using the HTTP inference proxy.
pub struct GrokBuildProvider {
    id: String,
    label: String,
    base_url: String,
    endpoint: String,
    model: String,
    reasoning_effort: String,
    context_window_tokens: Option<u64>,
    credentials: Arc<Mutex<Option<GrokCredentials>>>,
}

#[derive(Clone, Debug)]
struct GrokCredentials {
    access_token: String,
    refresh_token: Option<String>,
    expires_at: Option<SystemTime>,
    token_endpoint: Option<String>,
}

impl GrokBuildProvider {
    pub fn from_entry(id: &str, entry: &ProviderEntry) -> Self {
        let label = if entry.label.is_empty() {
            id.to_string()
        } else {
            entry.label.clone()
        };
        Self {
            id: id.to_string(),
            label,
            base_url: if entry.base_url.trim().is_empty() {
                DEFAULT_BASE_URL.to_string()
            } else {
                entry.base_url.trim_end_matches('/').to_string()
            },
            endpoint: if entry.endpoint.trim().is_empty() {
                DEFAULT_ENDPOINT.to_string()
            } else {
                entry.endpoint.clone()
            },
            model: if entry.model.is_empty() {
                DEFAULT_MODEL.to_string()
            } else {
                entry.model.clone()
            },
            reasoning_effort: entry.reasoning_effort.clone(),
            context_window_tokens: entry.context_window_tokens,
            credentials: Arc::new(Mutex::new(None)),
        }
    }

    async fn access_token(&self) -> Result<String, LlmError> {
        let mut credentials = self.credentials.lock().await;
        let mut current = credentials.clone().or_else(|| load_grok_credentials().ok());

        let Some(mut current_value) = current.take() else {
            return Err(LlmError::new_with_kind(
                LlmErrorKind::Auth,
                "Grok subscription OAuth credentials were not found; run `grok login` first",
            ));
        };

        if is_expiring(current_value.expires_at) {
            if let Some(refresh_token) = current_value.refresh_token.clone() {
                current_value =
                    refresh_grok_token(current_value.token_endpoint.as_deref(), &refresh_token)
                        .await?;
                // Persist so a rotated refresh token (and the new access token)
                // survive process restart. Best-effort: a disk write failure
                // must not abort an otherwise successful refresh for this turn.
                let _ = persist_grok_credentials(&current_value);
            } else if current_value.expires_at.is_some() {
                return Err(LlmError::new_with_kind(
                    LlmErrorKind::Auth,
                    "Grok OAuth access token expired; run `grok login` again",
                ));
            }
        }

        let token = current_value.access_token.clone();
        *credentials = Some(current_value);
        Ok(token)
    }

    fn entry(&self) -> ProviderEntry {
        ProviderEntry {
            label: self.label.clone(),
            base_url: self.base_url.clone(),
            model: self.model.clone(),
            // The Chat Completions adapter uses this as an explicit bearer override;
            // the subscription token is loaded immediately before each turn.
            api_key: String::new(),
            endpoint: self.endpoint.clone(),
            handler: "openai".to_string(),
            context_window_tokens: self.context_window_tokens,
            reasoning_effort: self.reasoning_effort.clone(),
        }
    }
}

/// Whether Bone can see a cached subscription credential without exposing it.
pub fn has_cached_auth() -> bool {
    load_grok_credentials().is_ok()
}

#[async_trait]
impl LlmProvider for GrokBuildProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.label
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn set_model(&mut self, model: String) {
        self.model = model;
    }

    fn context_window_tokens(&self) -> Option<u64> {
        self.context_window_tokens
    }

    async fn validate(&self) -> Result<(), LlmError> {
        self.access_token().await.map(|_| ())
    }

    async fn chat_stream(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<ResponseStream, LlmError> {
        self.chat_stream_with_context(messages, tools, ProviderRequestContext::default())
            .await
    }

    async fn chat_stream_with_context(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
        context: ProviderRequestContext,
    ) -> Result<ResponseStream, LlmError> {
        let token = self.access_token().await?;
        let client_version = std::env::var("GROK_CLI_VERSION")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_CLIENT_VERSION.to_string());
        let user_agent =
            format!("grok-pager/{client_version} grok-shell/{client_version} (linux; x86_64)");
        let headers = vec![
            ("User-Agent".to_string(), user_agent),
            (
                "x-grok-client-identifier".to_string(),
                "grok-pager".to_string(),
            ),
            ("x-grok-client-version".to_string(), client_version),
            ("x-xai-token-auth".to_string(), "xai-grok-cli".to_string()),
            ("x-grok-model-override".to_string(), self.model.clone()),
        ];
        let adapter = OpenAiCompatProvider::from_entry_with_transport(
            &self.id,
            &self.entry(),
            token,
            headers,
            Some(("x-grok-conv-id".to_string(), grok_conversation_id)),
        );
        adapter
            .chat_stream_with_context(messages, tools, context)
            .await
    }
}

/// Stable UUID-shaped conversation id expected by the Grok CLI proxy.
fn grok_conversation_id(conversation_id: i64) -> String {
    format!("00000000-0000-4000-8000-{:012x}", conversation_id as u64)
}

fn load_grok_credentials() -> Result<GrokCredentials, LlmError> {
    if let Some(access_token) = std::env::var("GROK_CLI_OAUTH_TOKEN")
        .ok()
        .filter(|value| !value.trim().is_empty())
    {
        return Ok(GrokCredentials {
            expires_at: jwt_expiry(&access_token),
            access_token,
            refresh_token: None,
            token_endpoint: None,
        });
    }

    load_grok_credentials_from(&grok_auth_path())
}

fn load_grok_credentials_from(path: &std::path::Path) -> Result<GrokCredentials, LlmError> {
    let data = std::fs::read_to_string(path).map_err(|error| {
        LlmError::new_with_kind(
            LlmErrorKind::Auth,
            format!(
                "could not read Grok OAuth credentials at {}: {error}",
                path.display()
            ),
        )
    })?;
    let document: Value = serde_json::from_str(&data).map_err(|error| {
        LlmError::new_with_kind(
            LlmErrorKind::Auth,
            format!("Grok OAuth credential file is invalid JSON: {error}"),
        )
    })?;

    let mut found = CredentialFields::default();
    find_credential_fields(&document, &mut found);
    let Some(access_token) = found.access_token else {
        return Err(LlmError::new_with_kind(
            LlmErrorKind::Auth,
            format!(
                "Grok OAuth credential file at {} has no access token",
                path.display()
            ),
        ));
    };
    Ok(GrokCredentials {
        expires_at: found.expires_at.or_else(|| jwt_expiry(&access_token)),
        access_token,
        refresh_token: found.refresh_token,
        token_endpoint: found.token_endpoint,
    })
}

fn grok_auth_path() -> PathBuf {
    if let Ok(home) = std::env::var("GROK_HOME") {
        return PathBuf::from(home).join("auth.json");
    }
    dirs::home_dir()
        .unwrap_or_default()
        .join(".grok")
        .join("auth.json")
}

/// Write refreshed OAuth fields back into the Grok CLI `auth.json` document.
///
/// The Grok CLI stores credentials under an OIDC entry object with `key`
/// (access token), `refresh_token`, and `expires_at`. We update those fields
/// in place so a rotated refresh token is not lost on the next process start.
fn persist_grok_credentials(creds: &GrokCredentials) -> Result<(), LlmError> {
    persist_grok_credentials_at(&grok_auth_path(), creds)
}

fn persist_grok_credentials_at(
    path: &std::path::Path,
    creds: &GrokCredentials,
) -> Result<(), LlmError> {
    let data = std::fs::read_to_string(path).map_err(|error| {
        LlmError::new_with_kind(
            LlmErrorKind::Auth,
            format!(
                "could not read Grok OAuth credentials at {} to persist refresh: {error}",
                path.display()
            ),
        )
    })?;
    let mut document: Value = serde_json::from_str(&data).map_err(|error| {
        LlmError::new_with_kind(
            LlmErrorKind::Auth,
            format!("Grok OAuth credential file is invalid JSON: {error}"),
        )
    })?;

    if !update_credential_fields(&mut document, creds) {
        return Err(LlmError::new_with_kind(
            LlmErrorKind::Auth,
            format!(
                "Grok OAuth credential file at {} has no credential entry to update",
                path.display()
            ),
        ));
    }

    let serialized = serde_json::to_string_pretty(&document).map_err(|error| {
        LlmError::new_with_kind(
            LlmErrorKind::Auth,
            format!("failed to serialize Grok OAuth credentials: {error}"),
        )
    })?;
    // Atomic replace so a crash mid-write cannot truncate auth.json.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &serialized).map_err(|error| {
        LlmError::new_with_kind(
            LlmErrorKind::Auth,
            format!(
                "could not write Grok OAuth credentials to {}: {error}",
                tmp.display()
            ),
        )
    })?;
    std::fs::rename(&tmp, path).map_err(|error| {
        LlmError::new_with_kind(
            LlmErrorKind::Auth,
            format!(
                "could not replace Grok OAuth credentials at {}: {error}",
                path.display()
            ),
        )
    })?;
    Ok(())
}

/// Locate the first object that holds access/refresh token fields and patch
/// them with the refreshed credentials. Returns whether an entry was updated.
fn update_credential_fields(value: &mut Value, creds: &GrokCredentials) -> bool {
    match value {
        Value::Object(map) => {
            let has_access = map.contains_key("key")
                || map.contains_key("access_token")
                || map.contains_key("accessToken");
            let has_refresh = map.contains_key("refresh_token")
                || map.contains_key("refreshToken")
                || map.contains_key("refresh");
            if has_access || has_refresh {
                // Prefer the field names already present so we do not introduce
                // a second access-token key alongside Grok CLI's `key`.
                if map.contains_key("key") {
                    map.insert("key".into(), Value::String(creds.access_token.clone()));
                } else if map.contains_key("accessToken") {
                    map.insert(
                        "accessToken".into(),
                        Value::String(creds.access_token.clone()),
                    );
                } else {
                    map.insert(
                        "access_token".into(),
                        Value::String(creds.access_token.clone()),
                    );
                }

                if let Some(refresh) = &creds.refresh_token {
                    if map.contains_key("refreshToken") {
                        map.insert("refreshToken".into(), Value::String(refresh.clone()));
                    } else {
                        map.insert("refresh_token".into(), Value::String(refresh.clone()));
                    }
                }

                if let Some(expires_at) = creds.expires_at {
                    let secs = expires_at
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    // Grok CLI stores RFC3339 strings; keep that shape when the
                    // existing field is a string (or missing).
                    let existing_is_string = map
                        .get("expires_at")
                        .or_else(|| map.get("expiresAt"))
                        .map(|v| v.is_string())
                        .unwrap_or(true);
                    let expiry_value = if existing_is_string {
                        Value::String(format_unix_rfc3339(secs))
                    } else {
                        Value::from(secs)
                    };
                    if map.contains_key("expiresAt") {
                        map.insert("expiresAt".into(), expiry_value);
                    } else {
                        map.insert("expires_at".into(), expiry_value);
                    }
                }
                return true;
            }

            for child in map.values_mut() {
                if update_credential_fields(child, creds) {
                    return true;
                }
            }
            false
        }
        Value::Array(values) => {
            for child in values {
                if update_credential_fields(child, creds) {
                    return true;
                }
            }
            false
        }
        _ => false,
    }
}

/// Minimal UTC RFC3339 formatter (`YYYY-MM-DDTHH:MM:SSZ`) without pulling in chrono.
fn format_unix_rfc3339(secs: u64) -> String {
    // Civil calendar conversion (proleptic Gregorian, UTC).
    let days = (secs / 86_400) as i64;
    let tod = secs % 86_400;
    let hour = tod / 3600;
    let min = (tod % 3600) / 60;
    let sec = tod % 60;

    // Days since 1970-01-01 → year/month/day (Howard Hinnant algorithm).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02}T{hour:02}:{min:02}:{sec:02}Z")
}

#[derive(Default)]
struct CredentialFields {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_at: Option<SystemTime>,
    token_endpoint: Option<String>,
}

fn find_credential_fields(value: &Value, found: &mut CredentialFields) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                match key.as_str() {
                    "access_token" | "accessToken" if found.access_token.is_none() => {
                        found.access_token = value
                            .as_str()
                            .filter(|token| !token.is_empty())
                            .map(ToOwned::to_owned);
                    }
                    "key" | "token" if found.access_token.is_none() => {
                        found.access_token = value
                            .as_str()
                            .filter(|token| token.len() > 20)
                            .map(ToOwned::to_owned);
                    }
                    "refresh_token" | "refreshToken" | "refresh"
                        if found.refresh_token.is_none() =>
                    {
                        found.refresh_token = value.as_str().map(ToOwned::to_owned);
                    }
                    "expires_at" | "expiresAt" | "expires" if found.expires_at.is_none() => {
                        found.expires_at = parse_expiry(value);
                    }
                    "token_endpoint" | "tokenEndpoint" if found.token_endpoint.is_none() => {
                        found.token_endpoint = value.as_str().map(ToOwned::to_owned);
                    }
                    _ => {}
                }
                find_credential_fields(value, found);
            }
        }
        Value::Array(values) => {
            for value in values {
                find_credential_fields(value, found);
            }
        }
        _ => {}
    }
}

fn parse_expiry(value: &Value) -> Option<SystemTime> {
    if let Some(seconds) = value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.parse::<u64>().ok()))
    {
        let seconds = if seconds > 10_000_000_000 {
            seconds / 1000
        } else {
            seconds
        };
        return Some(UNIX_EPOCH + Duration::from_secs(seconds));
    }
    value.as_str().and_then(parse_rfc3339_expiry)
}

/// Parse a subset of RFC3339 used by Grok CLI (`YYYY-MM-DDTHH:MM:SS[.frac]Z`).
fn parse_rfc3339_expiry(text: &str) -> Option<SystemTime> {
    let text = text.trim();
    let text = text.strip_suffix('Z').unwrap_or(text);
    let (date, time) = text.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: u64 = date_parts.next()?.parse().ok()?;
    let day: u64 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // Drop fractional seconds if present.
    let time = time.split('.').next().unwrap_or(time);
    // Drop timezone offsets like +00:00 if present (Grok CLI uses Z).
    let time = time.split(['+', '-']).next().unwrap_or(time);
    let mut time_parts = time.split(':');
    let hour: u64 = time_parts.next()?.parse().ok()?;
    let min: u64 = time_parts.next()?.parse().ok()?;
    let sec: u64 = time_parts.next()?.parse().ok()?;
    if time_parts.next().is_some() || hour > 23 || min > 59 || sec > 60 {
        return None;
    }
    let days = days_from_civil(year, month, day)?;
    let secs = days
        .checked_mul(86_400)?
        .checked_add((hour * 3600 + min * 60 + sec) as i64)?;
    if secs < 0 {
        return None;
    }
    Some(UNIX_EPOCH + Duration::from_secs(secs as u64))
}

/// Civil date → days since Unix epoch (Howard Hinnant).
fn days_from_civil(year: i64, month: u64, day: u64) -> Option<i64> {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe as i64 - 719_468)
}

fn jwt_expiry(token: &str) -> Option<SystemTime> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    parse_expiry(value.get("exp")?)
}

fn is_expiring(expires_at: Option<SystemTime>) -> bool {
    expires_at.is_some_and(|expiry| {
        expiry
            <= SystemTime::now()
                .checked_add(REFRESH_SKEW)
                .unwrap_or(SystemTime::now())
    })
}

async fn refresh_grok_token(
    token_endpoint: Option<&str>,
    refresh_token: &str,
) -> Result<GrokCredentials, LlmError> {
    let endpoint = match token_endpoint {
        Some(endpoint) if !endpoint.is_empty() => endpoint.to_string(),
        _ => discover_token_endpoint().await?,
    };
    let response = reqwest::Client::new()
        .post(&endpoint)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", OAUTH_CLIENT_ID),
            ("refresh_token", refresh_token),
        ])
        .send()
        .await
        .map_err(|error| {
            LlmError::new_with_kind(
                LlmErrorKind::Connection,
                format!("refreshing Grok OAuth: {error}"),
            )
        })?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(LlmError::new_with_kind(
            LlmErrorKind::Auth,
            format!(
                "Grok OAuth refresh failed with HTTP {status}: {}",
                body.trim()
            ),
        ));
    }
    let value: Value = serde_json::from_str(&body).map_err(|error| {
        LlmError::new_with_kind(
            LlmErrorKind::Auth,
            format!("invalid Grok OAuth refresh response: {error}"),
        )
    })?;
    let access_token = value
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| {
            LlmError::new_with_kind(
                LlmErrorKind::Auth,
                "Grok OAuth refresh response did not contain access_token",
            )
        })?
        .to_string();
    // Store true expiry; `is_expiring` applies REFRESH_SKEW at check time so
    // disk/memory agree and we do not double-skew.
    let expires_at = value
        .get("expires_in")
        .and_then(Value::as_u64)
        .map(|seconds| {
            SystemTime::now()
                .checked_add(Duration::from_secs(seconds))
                .unwrap_or(SystemTime::now())
        })
        .or_else(|| jwt_expiry(&access_token));
    Ok(GrokCredentials {
        access_token,
        refresh_token: value
            .get("refresh_token")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .or_else(|| Some(refresh_token.to_string())),
        expires_at,
        token_endpoint: Some(endpoint),
    })
}

async fn discover_token_endpoint() -> Result<String, LlmError> {
    let url = format!("{OIDC_ISSUER}{OIDC_DISCOVERY_PATH}");
    let value: Value = reqwest::get(&url)
        .await
        .map_err(|error| {
            LlmError::new_with_kind(
                LlmErrorKind::Connection,
                format!("discovering Grok OAuth: {error}"),
            )
        })?
        .error_for_status()
        .map_err(|error| {
            LlmError::new_with_kind(
                LlmErrorKind::Auth,
                format!("Grok OAuth discovery failed: {error}"),
            )
        })?
        .json()
        .await
        .map_err(|error| {
            LlmError::new_with_kind(
                LlmErrorKind::Auth,
                format!("invalid Grok OAuth discovery response: {error}"),
            )
        })?;
    value
        .get("token_endpoint")
        .and_then(Value::as_str)
        .filter(|endpoint| endpoint.starts_with("https://") && endpoint.contains("x.ai"))
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            LlmError::new_with_kind(
                LlmErrorKind::Auth,
                "Grok OAuth discovery did not provide a trusted token endpoint",
            )
        })
}

#[cfg(test)]
#[path = "grok_build_tests.rs"]
mod grok_build_tests;

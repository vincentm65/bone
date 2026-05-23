//! Codex-specific authentication helpers.

use serde_json::Value;
use std::path::Path;

/// Read access token from Codex CLI's cached auth (`~/.codex/auth.json`).
///
/// Returns the `access_token` if available, empty string otherwise.
pub fn read_codex_token() -> String {
    let path = Path::new(&dirs::home_dir().unwrap_or_default()).join(".codex/auth.json");
    let Ok(data) = std::fs::read_to_string(&path) else {
        return String::new();
    };
    let Ok(doc): Result<Value, _> = serde_json::from_str(&data) else {
        return String::new();
    };
    doc["tokens"]["access_token"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

/// Resolve the API key: prefer the dynamic Codex CLI token, fall back to config.
pub fn resolve_codex_api_key(config_key: &str) -> String {
    let token = read_codex_token();
    if !token.is_empty() {
        return token;
    }
    config_key.to_string()
}

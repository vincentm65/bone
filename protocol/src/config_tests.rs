use super::*;

#[test]
fn redacted_provider_never_serializes_a_secret_field() {
    let provider = ProviderConfig {
        id: "openai".into(),
        label: "OpenAI".into(),
        base_url: "https://api.openai.com".into(),
        model: "gpt".into(),
        endpoint: "/chat/completions".into(),
        handler: "openai".into(),
        context_window_tokens: None,
        max_concurrency: None,
        reasoning_effort: String::new(),
        fast_mode: false,
        supports_prompt_cache_key: true,
        stream_usage: "auto".into(),
        request_timeout_s: None,
        api_key_configured: true,
        api_key_required: true,
    };
    let json = serde_json::to_value(provider).unwrap();
    assert!(json.get("api_key").is_none());
    assert_eq!(json["api_key_configured"], true);
    assert_eq!(json["api_key_required"], true);
}

#[test]
fn provider_key_capability_defaults_to_required_for_old_daemon_payloads() {
    let provider = ProviderConfig {
        id: "local".into(),
        label: "Local".into(),
        base_url: "http://localhost:8080".into(),
        model: "model".into(),
        endpoint: "/chat/completions".into(),
        handler: "openai_compat".into(),
        context_window_tokens: None,
        max_concurrency: None,
        reasoning_effort: String::new(),
        fast_mode: false,
        supports_prompt_cache_key: false,
        stream_usage: "auto".into(),
        request_timeout_s: None,
        api_key_configured: false,
        api_key_required: false,
    };
    let mut old_payload = serde_json::to_value(provider).unwrap();
    old_payload
        .as_object_mut()
        .expect("provider serializes as an object")
        .remove("api_key_required");

    let decoded: ProviderConfig = serde_json::from_value(old_payload).unwrap();
    assert!(decoded.api_key_required);
}

#[test]
fn omitted_provider_key_preserves_update_intent() {
    let update: ProviderUpdate = serde_json::from_value(serde_json::json!({
        "id": "local",
        "label": "Local",
        "base_url": "http://localhost:8080",
        "model": "model",
        "endpoint": "/chat/completions",
        "handler": "openai",
        "context_window_tokens": null,
        "reasoning_effort": ""
    }))
    .unwrap();
    assert_eq!(update.max_concurrency, None);
    assert_eq!(update.fast_mode, None);
    assert_eq!(update.supports_prompt_cache_key, None);
    assert_eq!(update.stream_usage, None);
    assert_eq!(update.api_key, None);
}

use super::*;

#[test]
fn reasoning_effort_presets_are_canonical_and_custom_values_pass_through() {
    let mut entry: ProviderEntry = serde_yaml::from_str("reasoning_effort: HIGH").unwrap();
    assert_eq!(entry.reasoning_effort_opt().as_deref(), Some("high"));
    entry.reasoning_effort = " ultra ".into();
    assert_eq!(entry.reasoning_effort_opt().as_deref(), Some("ultra"));
    entry.reasoning_effort = "FutureMode".into();
    assert_eq!(entry.reasoning_effort_opt().as_deref(), Some("FutureMode"));
    entry.reasoning_effort = "DEFAULT".into();
    assert_eq!(entry.reasoning_effort_opt(), None);
}

#[test]
fn stream_usage_defaults_to_auto_and_normalizes_tri_state() {
    let entry: ProviderEntry = serde_yaml::from_str("base_url: https://example.com").unwrap();
    assert_eq!(entry.stream_usage, "auto");

    let entry: ProviderEntry = serde_yaml::from_str("stream_usage: TRUE").unwrap();
    assert_eq!(entry.stream_usage, "true");
    let entry: ProviderEntry = serde_yaml::from_str("stream_usage: Auto").unwrap();
    assert_eq!(entry.stream_usage, "auto");

    // Plain booleans are accepted as well.
    let entry: ProviderEntry = serde_yaml::from_str("stream_usage: true").unwrap();
    assert_eq!(entry.stream_usage, "true");
    let entry: ProviderEntry = serde_yaml::from_str("stream_usage: false").unwrap();
    assert_eq!(entry.stream_usage, "false");

    assert!(serde_yaml::from_str::<ProviderEntry>("stream_usage: maybe").is_err());
}

#[test]
fn stream_usage_auto_is_skipped_when_serialized() {
    let entry: ProviderEntry = serde_yaml::from_str("stream_usage: auto").unwrap();
    let yaml = serde_yaml::to_string(&entry).unwrap();
    assert!(!yaml.contains("stream_usage"));
    let entry: ProviderEntry = serde_yaml::from_str("stream_usage: \"true\"").unwrap();
    let yaml = serde_yaml::to_string(&entry).unwrap();
    assert!(yaml.contains("stream_usage: 'true'"));
}

#[test]
fn stream_usage_enabled_resolves_tri_state_over_host_list() {
    let mut entry: ProviderEntry =
        serde_yaml::from_str("base_url: https://api.openai.com/v1").unwrap();
    assert!(entry.stream_usage_enabled());
    entry.stream_usage = "false".into();
    assert!(!entry.stream_usage_enabled());

    let mut entry: ProviderEntry =
        serde_yaml::from_str("base_url: https://openrouter.ai/api/v1").unwrap();
    assert!(!entry.stream_usage_enabled());
    entry.stream_usage = "true".into();
    assert!(entry.stream_usage_enabled());
}

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

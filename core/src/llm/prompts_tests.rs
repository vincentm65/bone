use super::*;

#[test]
fn default_prompt_includes_builtin_base_and_runtime_context() {
    let prompt = system_prompt();
    assert!(prompt.starts_with(SYSTEM_PROMPT));
    assert!(prompt.contains("Resolved config directory: "));
    assert!(prompt.contains("Current working directory: "));
}

#[test]
fn configured_prompt_replaces_only_the_builtin_base() {
    let prompt = system_prompt_with_base(Some("Custom main-agent instructions."));
    assert!(prompt.starts_with("Custom main-agent instructions.\n\n"));
    assert!(!prompt.contains("You are bone, a coding assistant"));
    assert!(prompt.contains("Resolved config directory: "));
    assert!(prompt.contains("Current working directory: "));
}

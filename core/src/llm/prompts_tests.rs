use super::*;

#[test]
fn shipped_prompt_includes_configured_base_and_runtime_context() {
    let base = crate::config::settings::shipped_system_prompt();
    let prompt = system_prompt(base);
    assert!(prompt.starts_with(base));
    assert!(prompt.contains("Resolved config directory: "));
    assert!(prompt.contains("Current working directory: "));
}

#[test]
fn configured_prompt_gets_only_runtime_context_appended() {
    let prompt = system_prompt("Custom main-agent instructions.");
    assert!(prompt.starts_with("Custom main-agent instructions.\n\n"));
    assert!(!prompt.contains("You are bone, a coding assistant"));
    assert!(prompt.contains("Resolved config directory: "));
    assert!(prompt.contains("Current working directory: "));
}

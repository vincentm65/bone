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
fn configured_prompt_gets_tool_guidance_and_runtime_context_appended() {
    let prompt = system_prompt("Custom main-agent instructions.");
    assert!(prompt.starts_with("Custom main-agent instructions.\n\n"));
    assert!(!prompt.contains("You are bone, a coding assistant"));
    assert!(prompt.contains("multiple independent tool calls in a single turn"));
    assert!(prompt.contains("batch related reads/searches"));
    assert!(
        prompt.contains("edit_file accepts several disjoint replacements in one call via edits")
    );
    assert!(prompt.contains("Do not re-read a file you just changed"));
    assert!(prompt.contains("Resolved config directory: "));
    assert!(prompt.contains("Current working directory: "));
}

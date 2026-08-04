use super::*;

#[test]
fn only_builtin_commands_are_protected() {
    for command in [
        "catalog",
        "clear",
        "config",
        "edit",
        "e",
        "exit",
        "help",
        "incognito",
        "model",
        "new",
        "provider",
        "quit",
        "setup",
        "stats",
        "tools",
        "update",
    ] {
        assert!(
            is_protected_builtin(command),
            "/{command} should be protected"
        );
    }

    for command in ["agents", "compact", "context", "usage", "memory", "review"] {
        assert!(
            !is_protected_builtin(command),
            "/{command} should be overridable"
        );
    }
}

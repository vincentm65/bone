use super::*;

#[test]
fn split_editor_command_keeps_args() {
    assert_eq!(split_editor_command("code -w"), vec!["code", "-w"]);
}

#[test]
fn split_editor_command_respects_quotes() {
    assert_eq!(
        split_editor_command("\"/opt/Editor With Spaces/editor\" --wait"),
        vec!["/opt/Editor With Spaces/editor", "--wait"]
    );
}

#[test]
fn default_editor_is_platform_specific() {
    if cfg!(windows) {
        assert_eq!(default_editor(), "notepad");
    } else {
        assert_eq!(default_editor(), "nano");
    }
}

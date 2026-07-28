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

#[test]
fn editor_temp_paths_are_unique_and_cleaned_up() {
    let first = editor_temp_path().unwrap();
    let first_path = first.to_path_buf();
    let second = editor_temp_path().unwrap();
    let second_path = second.to_path_buf();

    assert_ne!(first_path, second_path);
    assert!(first_path.exists());
    assert!(second_path.exists());

    drop(first);
    drop(second);
    assert!(!first_path.exists());
    assert!(!second_path.exists());
}

use super::*;

#[test]
fn image_paste_shortcuts_do_not_capture_terminal_text_paste() {
    assert!(is_image_paste_key(
        KeyCode::Char('v'),
        KeyModifiers::CONTROL
    ));
    assert!(is_image_paste_key(KeyCode::Char('v'), KeyModifiers::ALT));
    assert!(!is_image_paste_key(
        KeyCode::Char('V'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT
    ));
    assert!(!is_image_paste_key(KeyCode::Char('v'), KeyModifiers::NONE));
}

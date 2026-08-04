use super::*;

#[test]
fn image_paste_shortcuts_accept_shifted_v() {
    assert!(is_image_paste_key(
        KeyCode::Char('V'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT
    ));
    assert!(is_image_paste_key(
        KeyCode::Char('v'),
        KeyModifiers::CONTROL
    ));
    assert!(!is_image_paste_key(KeyCode::Char('v'), KeyModifiers::NONE));
}

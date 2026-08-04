use super::*;

#[test]
fn input_style_defaults() {
    let snapshot = InputStyleSnapshot::default();
    assert!(snapshot.preset.is_none());
    assert!(snapshot.prefix.is_none());
    assert!(snapshot.border.horizontal.is_none());
}

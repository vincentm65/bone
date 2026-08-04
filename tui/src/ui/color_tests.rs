use super::*;

#[test]
fn rgb_conversion_rejects_terminal_dependent_colors() {
    assert_eq!(color_to_rgb(Color::Rgb(1, 2, 3)), Some((1, 2, 3)));
    assert_eq!(color_to_rgb(Color::Cyan), Some((0x11, 0xA8, 0xCD)));
    assert_eq!(color_to_rgb(Color::Indexed(42)), None);
    assert_eq!(color_to_rgb(Color::Reset), None);
}

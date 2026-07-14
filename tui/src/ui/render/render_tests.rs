use super::*;

#[test]
fn terminal_color_rgb_maps_truecolor_and_named_colors() {
    assert_eq!(terminal_color_rgb(Color::Rgb(1, 2, 3)), (1, 2, 3));
    assert_eq!(terminal_color_rgb(Color::Black), (0, 0, 0));
    assert_eq!(terminal_color_rgb(Color::White), (255, 255, 255));
    assert_eq!(terminal_color_rgb(Color::LightBlue), (0x3B, 0x8E, 0xEA));
}

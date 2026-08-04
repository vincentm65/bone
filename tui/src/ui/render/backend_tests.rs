use super::*;
use ratatui::{buffer::Buffer, layout::Rect, style::Style};

#[test]
fn full_width_background_uses_erase_to_end_instead_of_padding_spaces() {
    let width = 80;
    let area = Rect::new(0, 0, width, 1);
    let mut next = Buffer::empty(area);
    next.set_style(area, Style::default().bg(Color::DarkGray));
    next.set_string(
        0,
        0,
        "> text",
        Style::default().fg(Color::White).bg(Color::DarkGray),
    );
    let previous = Buffer::empty(area);
    let mut backend = BoneBackend::new(Vec::<u8>::new());

    backend
        .draw_with_width(previous.diff(&next).into_iter(), width)
        .unwrap();
    Backend::flush(&mut backend).unwrap();
    let output = String::from_utf8_lossy(backend.inner.writer());

    assert!(
        output.contains("\u{1b}[K"),
        "expected erase-to-line-end: {output:?}"
    );
    assert!(
        !output.contains("    "),
        "printed background padding: {output:?}"
    );
}

use bone::ui::render::wrap::{visual_line_count, wrap_text, wrap_text_with_prefix};

#[test]
fn wraps_long_line_at_words() {
    assert_eq!(
        wrap_text("alpha beta gamma", 10),
        vec!["alpha", " beta", " gamma"]
    );
}

#[test]
fn hard_wraps_long_words() {
    assert_eq!(wrap_text("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
}

#[test]
fn wraps_with_prefixes() {
    assert_eq!(
        wrap_text_with_prefix("alpha beta gamma", "> ", "  ", 10),
        vec!["> alpha", "  beta", "  gamma"]
    );
}

#[test]
fn preserves_empty_line_with_prefix() {
    assert_eq!(wrap_text_with_prefix("", "> ", "  ", 10), vec!["> "]);
}

#[test]
fn handles_tiny_width() {
    assert_eq!(
        wrap_text_with_prefix("abc", "> ", "  ", 1),
        vec!["> a", "  b", "  c"]
    );
}

#[test]
fn uses_display_width_for_wide_chars() {
    assert_eq!(wrap_text("你好世界", 4), vec!["你好", "世界"]);
}

#[test]
fn counts_visual_lines() {
    assert_eq!(visual_line_count("", 10), 1);
    assert_eq!(visual_line_count("hello", 10), 1);
    assert_eq!(visual_line_count("hello world", 5), 3);
    assert_eq!(visual_line_count("a\nb", 10), 2);
    assert_eq!(visual_line_count("a\nb\nc", 10), 3);
}

#[test]
fn counts_visual_lines_with_wide_chars() {
    // Each CJK char is width 2; 4 chars = width 8, at width 4 => 2 lines
    assert_eq!(visual_line_count("你好世界", 4), 2);
}

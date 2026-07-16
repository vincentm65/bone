use bone::ui::render::wrap::{wrap_text, wrap_text_with_prefix};

#[test]
fn wraps_long_line_at_words() {
    assert_eq!(
        wrap_text("alpha beta gamma", 10),
        vec!["alpha", " beta", " gamma"]
    );
}

#[test]
fn wrapped_indented_line_keeps_indent_on_continuations() {
    assert_eq!(
        wrap_text("  alpha beta gamma", 10),
        vec!["  alpha", "  beta", "  gamma"]
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

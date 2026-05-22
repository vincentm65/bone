use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Wrap one logical line into terminal-width visual lines.
pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    wrap_plain_line(text, width.max(1))
}

/// Wrap one logical line while applying prefixes to the first and continuation lines.
pub fn wrap_text_with_prefix(
    text: &str,
    first_prefix: &str,
    rest_prefix: &str,
    width: usize,
) -> Vec<String> {
    let width = width.max(1);
    let first_prefix_width = UnicodeWidthStr::width(first_prefix);
    let rest_prefix_width = UnicodeWidthStr::width(rest_prefix);
    let first_width = width.saturating_sub(first_prefix_width).max(1);
    let rest_width = width.saturating_sub(rest_prefix_width).max(1);

    if text.is_empty() {
        return vec![first_prefix.to_string()];
    }

    let first_take = take_breakable_width(text, first_width);
    let (first, mut rest) = text.split_at(first_take);
    let mut lines = vec![format!("{first_prefix}{first}")];

    while !rest.is_empty() {
        let take = take_breakable_width(rest, rest_width);
        let (line, remaining) = rest.split_at(take);
        lines.push(format!("{rest_prefix}{}", line.trim_start()));
        rest = remaining.trim_start();
    }

    lines
}

fn wrap_plain_line(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }

    let mut lines = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let take = take_breakable_width(rest, width);
        let (line, remaining) = rest.split_at(take);
        lines.push(line.to_string());
        rest = remaining;
    }
    lines
}

/// How many visual lines does `text` need at `width` display columns?
/// Handles hard newlines and uses Unicode display width (correct for CJK).
pub fn visual_line_count(text: &str, width: usize) -> usize {
    let width = width.max(1);
    if text.is_empty() {
        return 1;
    }
    text.split('\n')
        .map(|line| {
            if line.is_empty() {
                return 1;
            }
            let w = UnicodeWidthStr::width(line);
            w.div_ceil(width).max(1)
        })
        .sum()
}

/// Return a byte index that fits within `width`, preferring the last whitespace break.
fn take_breakable_width(text: &str, width: usize) -> usize {
    let hard = take_width(text, width);
    if hard >= text.len() {
        return hard;
    }

    let candidate = &text[..hard];
    if let Some((idx, _)) = candidate
        .char_indices()
        .rev()
        .find(|(_, ch)| ch.is_whitespace())
        && idx > 0
    {
        return idx;
    }

    hard
}

/// Return the largest valid byte index whose display width is at most `width`.
fn take_width(text: &str, width: usize) -> usize {
    let mut used = 0;
    let mut end = 0;

    for (idx, ch) in text.char_indices() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + ch_width > width {
            return if end == 0 { idx + ch.len_utf8() } else { end };
        }
        used += ch_width;
        end = idx + ch.len_utf8();
    }

    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

//! Terminal-width text-wrapping helpers.

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

    let leading = text.len() - text.trim_start().len();
    if leading > 0 && leading < text.len() {
        let indent = &text[..leading];
        let content = &text[leading..];
        return wrap_text_with_prefix(content, indent, indent, width);
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
        .find(|(idx, ch)| *idx > 0 && ch.is_whitespace())
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

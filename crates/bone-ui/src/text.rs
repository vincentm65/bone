//! Text helpers: sanitizing untrusted output, display width, wrapping.

use unicode_width::UnicodeWidthChar;

const TAB_WIDTH: usize = 4;

/// Make text safe to put in terminal cells: strip ANSI escape sequences and
/// control characters, expand tabs. Newlines are kept.
pub fn sanitize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut col = 0;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => skip_escape(&mut chars),
            '\n' => {
                out.push('\n');
                col = 0;
            }
            '\t' => {
                let n = TAB_WIDTH - col % TAB_WIDTH;
                out.extend(std::iter::repeat_n(' ', n));
                col += n;
            }
            '\r' => {}
            c if c.is_control() => {}
            c => {
                out.push(c);
                col += c.width().unwrap_or(0);
            }
        }
    }
    out
}

fn skip_escape(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    match chars.next() {
        // CSI: parameters then one final byte in @..~
        Some('[') => {
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        }
        // OSC / DCS / APC...: until BEL or ESC \
        Some(']' | 'P' | '_' | '^' | 'X') => {
            while let Some(c) = chars.next() {
                if c == '\x07' {
                    break;
                }
                if c == '\x1b' && chars.peek() == Some(&'\\') {
                    chars.next();
                    break;
                }
            }
        }
        _ => {}
    }
}

/// Strip escape sequences and control characters, keeping newlines and
/// tabs. Text handed to Lua goes through this; [`sanitize`] (which also
/// expands tabs) runs on everything drawn.
pub fn clean(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => skip_escape(&mut chars),
            '\n' | '\t' => out.push(c),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Styled text as `(text, highlight group)` pairs: the form Lua works with.
pub type Groups = Vec<(String, String)>;

/// Word-wrap grouped spans to `width`; `first` prefixes the first row and
/// `rest` the others. Text is sanitized first (tabs expanded).
pub fn wrap_groups(
    spans: &[(String, String)],
    first: &[(String, String)],
    rest: &[(String, String)],
    width: usize,
) -> Vec<Groups> {
    let styled: Vec<(char, &str)> = spans
        .iter()
        .flat_map(|(t, g)| {
            sanitize(t)
                .chars()
                .filter(|c| *c != '\n')
                .map(|c| (c, g.as_str()))
                .collect::<Vec<_>>()
        })
        .collect();
    let chars: Vec<char> = styled.iter().map(|(c, _)| *c).collect();
    let prefix_w = |p: &[(String, String)]| p.iter().map(|(t, _)| self::width(t)).sum::<usize>();
    let body = width
        .saturating_sub(prefix_w(first).max(prefix_w(rest)))
        .max(1);
    wrap_ranges(&chars, body)
        .into_iter()
        .enumerate()
        .map(|(i, r)| {
            let mut body: Groups = Vec::new();
            for &(c, g) in &styled[r] {
                match body.last_mut() {
                    Some((t, last)) if last == g => t.push(c),
                    _ => body.push((c.to_string(), g.to_owned())),
                }
            }
            let mut out: Groups = if i == 0 {
                first.to_vec()
            } else {
                rest.to_vec()
            };
            out.extend(body);
            out
        })
        .collect()
}

/// Cut grouped spans to `width` columns, ending with `…` if cut.
pub fn clip_groups(spans: &[(String, String)], width: usize) -> Groups {
    let total: usize = spans.iter().map(|(t, _)| self::width(&sanitize(t))).sum();
    if total <= width {
        return spans.to_vec();
    }
    let mut out = Vec::new();
    let mut used = 0;
    for (t, g) in spans {
        let t = sanitize(t);
        let w = self::width(&t);
        if used + w < width {
            used += w;
            out.push((t, g.clone()));
        } else {
            out.push((truncate(&t, width - used), g.clone()));
            break;
        }
    }
    out
}

pub fn width(s: &str) -> usize {
    s.chars().map(|c| c.width().unwrap_or(0)).sum()
}

/// Word-wrap one line (no `\n`) to `width` columns. Always returns at least
/// one row. Words longer than a row are broken.
pub fn wrap(line: &str, width: usize) -> Vec<String> {
    let chars: Vec<char> = line.chars().collect();
    wrap_ranges(&chars, width)
        .into_iter()
        .map(|r| chars[r].iter().collect())
        .collect()
}

/// Row boundaries (char index ranges) for word-wrapping `chars`. Spaces at a
/// break are dropped.
pub fn wrap_ranges(chars: &[char], width: usize) -> Vec<std::ops::Range<usize>> {
    let width = width.max(1);
    let cw = |c: char| c.width().unwrap_or(0);
    let trimmed = |start: usize, mut end: usize| {
        while end > start && chars[end - 1] == ' ' {
            end -= 1;
        }
        start..end
    };
    let mut rows = Vec::new();
    let mut start = 0;
    let mut cur_w = 0;
    let mut last_space: Option<usize> = None;
    for (i, &c) in chars.iter().enumerate() {
        let w = cw(c);
        if c == ' ' && cur_w + w > width {
            // A space that does not fit is the break itself.
            rows.push(trimmed(start, i));
            start = i + 1;
            cur_w = 0;
            last_space = None;
            continue;
        }
        while cur_w + w > width && i > start {
            match last_space.take().filter(|&sp| sp >= start) {
                Some(sp) => {
                    rows.push(trimmed(start, sp));
                    start = sp + 1;
                    cur_w = chars[start..i].iter().map(|&c| cw(c)).sum();
                }
                None => {
                    rows.push(start..i);
                    start = i;
                    cur_w = 0;
                }
            }
        }
        cur_w += w;
        if c == ' ' {
            last_space = Some(i);
        }
    }
    rows.push(start..chars.len());
    rows
}

/// Cut `s` to at most `max` columns, adding `…` if it was cut.
pub fn truncate(s: &str, max: usize) -> String {
    if width(s) <= max {
        return s.to_owned();
    }
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw + 1 > max {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_escapes_and_controls() {
        assert_eq!(sanitize("\x1b[31mred\x1b[0m\r\n"), "red\n");
        assert_eq!(sanitize("\x1b]0;title\x07ok\x1b]8;;u\x1b\\x"), "okx");
        assert_eq!(sanitize("a\tb\x00\x08"), "a   b");
        assert_eq!(sanitize("  1\tx"), "  1 x");
    }

    #[test]
    fn wrap_breaks_on_spaces_and_long_words() {
        assert_eq!(wrap("hello big world", 9), vec!["hello big", "world"]);
        assert_eq!(wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(wrap("", 4), vec![""]);
        assert_eq!(wrap("ab 日本語", 4), vec!["ab", "日本", "語"]);
    }

    #[test]
    fn grouped_wrap_and_clip() {
        let g = |t: &str, h: &str| (t.to_owned(), h.to_owned());
        let rows = wrap_groups(
            &[g("aa ", "A"), g("bbb cc", "B")],
            &[g("> ", "P")],
            &[g("  ", "P")],
            8,
        );
        assert_eq!(
            rows,
            vec![
                vec![g("> ", "P"), g("aa ", "A"), g("bbb", "B")],
                vec![g("  ", "P"), g("cc", "B")]
            ]
        );
        // Text equal to a prefix's group still starts its own span.
        let rows = wrap_groups(&[g("x", "P")], &[g("> ", "P")], &[], 8);
        assert_eq!(rows, vec![vec![g("> ", "P"), g("x", "P")]]);
        assert_eq!(
            clip_groups(&[g("abc", "A"), g("def", "B")], 4),
            vec![g("abc", "A"), g("…", "B")]
        );
        assert_eq!(clean("a\x1b[31mb\tc\x07"), "ab\tc");
    }

    #[test]
    fn truncate_adds_ellipsis() {
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("abc", 4), "abc");
    }
}

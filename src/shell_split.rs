/// Options controlling how shell command text is split into segments/lines.
pub struct ShellSplitOptions {
    /// If true, include separator characters (`&&`, `||`, `|`, `;`) in the
    /// output segment rather than discarding them.
    pub keep_separators: bool,
    /// If true, treat newline as a separator (like `;`).
    pub split_newlines: bool,
    /// If true, strip `#`-comments that start at a word boundary.
    pub strip_comments: bool,
}

/// Split a shell command string into segments at unquoted separators.
///
/// Handles single/double quoting and backslash escaping. Separator characters
/// (`&&`, `||`, `|`, `;`, and optionally `\n`) are treated as segment
/// boundaries.
pub fn shell_split(command: &str, opts: &ShellSplitOptions) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut single = false;
    let mut double = false;
    let mut escaped = false;
    let mut at_word_start = true;
    let mut chars = command.chars().peekable();

    while let Some(ch) = chars.next() {
        if escaped {
            current.push(ch);
            escaped = false;
            at_word_start = ch.is_whitespace();
            continue;
        }
        if ch == '\\' {
            current.push(ch);
            escaped = true;
            at_word_start = false;
            continue;
        }
        if ch == '\'' && !double {
            single = !single;
            current.push(ch);
            at_word_start = false;
            continue;
        }
        if ch == '"' && !single {
            double = !double;
            current.push(ch);
            at_word_start = false;
            continue;
        }
        // Comment stripping: at word start, outside quotes, consume to newline.
        if opts.strip_comments && ch == '#' && !single && !double && at_word_start {
            for next in chars.by_ref() {
                if next == '\n' {
                    push_segment(&mut segments, &mut current);
                    at_word_start = true;
                    break;
                }
            }
            continue;
        }
        if !single && !double {
            let is_sep = match ch {
                '&' if chars.peek() == Some(&'&') => {
                    if opts.keep_separators {
                        current.push_str("&&");
                    }
                    chars.next();
                    true
                }
                '|' if chars.peek() == Some(&'|') => {
                    if opts.keep_separators {
                        current.push_str("||");
                    }
                    chars.next();
                    true
                }
                '|' | ';' => {
                    if opts.keep_separators {
                        current.push(ch);
                    }
                    true
                }
                '\n' if opts.split_newlines => true,
                _ => false,
            };
            if is_sep {
                push_segment(&mut segments, &mut current);
                at_word_start = true;
                continue;
            }
        }

        current.push(ch);
        at_word_start = ch.is_whitespace();
    }
    push_segment(&mut segments, &mut current);
    segments
}

fn push_segment(segments: &mut Vec<String>, segment: &mut String) {
    let trimmed = segment.trim();
    if !trimmed.is_empty() {
        segments.push(trimmed.to_string());
    }
    segment.clear();
}

#[cfg(test)]
#[path = "shell_split_tests.rs"]
mod shell_split_tests;

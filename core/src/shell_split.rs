//! Shell-command splitting into segments/lines for display and policy checks.

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

#[derive(Default)]
struct ShellState {
    single: bool,
    double: bool,
    escaped: bool,
    comment: bool,
    at_word_start: bool,
    /// Parenthesis depth for a command substitution, including its `$(`.
    /// Zero denotes the outer shell rather than a substitution.
    paren_depth: usize,
}

/// Split a shell command string into segments at unquoted separators.
///
/// Handles single/double quoting, backslash escaping, and `$( )` command
/// substitution nesting: a separator that belongs to a nested command is part of
/// that command, not of the surrounding one, so it does not split here. The body
/// of a substitution is passed through verbatim, `#` comments included, because
/// whoever inspects it (the destructive-command guard) re-splits and re-inspects
/// it as a command in its own right.
pub fn shell_split(command: &str, opts: &ShellSplitOptions) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut states = vec![ShellState {
        at_word_start: true,
        ..ShellState::default()
    }];
    let mut chars = command.chars().peekable();

    while let Some(ch) = chars.next() {
        let in_substitution = states.len() > 1;
        let state = states.last_mut().expect("outer shell state exists");

        if state.comment {
            current.push(ch);
            if ch == '\n' {
                state.comment = false;
                state.at_word_start = true;
            }
            continue;
        }
        if state.escaped {
            current.push(ch);
            state.escaped = false;
            // A backslash-quoted character is literal data: even when it is a
            // separator (`\;`, `\)`) or `#`, it continues the current word, so the
            // next `#` is still mid-word and does not start a comment.
            state.at_word_start = false;
            continue;
        }
        if ch == '\\' {
            current.push(ch);
            state.escaped = true;
            state.at_word_start = false;
            continue;
        }
        if ch == '\'' && !state.double {
            state.single = !state.single;
            current.push(ch);
            state.at_word_start = false;
            continue;
        }
        if ch == '"' && !state.single {
            state.double = !state.double;
            current.push(ch);
            state.at_word_start = false;
            continue;
        }
        if ch == '$' && !state.single && chars.peek() == Some(&'(') {
            chars.next();
            current.push_str("$(");
            state.at_word_start = false;
            states.push(ShellState {
                at_word_start: true,
                paren_depth: 1,
                ..ShellState::default()
            });
            continue;
        }

        if in_substitution {
            if !state.single && !state.double {
                if opts.strip_comments && ch == '#' && state.at_word_start {
                    current.push(ch);
                    state.comment = true;
                    state.at_word_start = false;
                    continue;
                }
                if ch == '(' {
                    state.paren_depth += 1;
                } else if ch == ')' {
                    state.paren_depth -= 1;
                    current.push(ch);
                    if state.paren_depth == 0 {
                        states.pop();
                        states
                            .last_mut()
                            .expect("substitution has an outer state")
                            .at_word_start = false;
                    } else {
                        state.at_word_start = true;
                    }
                    continue;
                }
            }
            current.push(ch);
            state.at_word_start = is_word_boundary(ch);
            continue;
        }

        // Comment stripping: at word start, outside quotes, consume to newline.
        if opts.strip_comments && ch == '#' && !state.single && !state.double && state.at_word_start {
            for next in chars.by_ref() {
                if next == '\n' {
                    push_segment(&mut segments, &mut current);
                    state.at_word_start = true;
                    break;
                }
            }
            continue;
        }
        if !state.single && !state.double {
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
                state.at_word_start = true;
                continue;
            }
        }

        current.push(ch);
        state.at_word_start = is_word_boundary(ch);
    }
    push_segment(&mut segments, &mut current);
    segments
}

fn is_word_boundary(ch: char) -> bool {
    ch.is_whitespace() || matches!(ch, '&' | '|' | ';' | '(' | ')')
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

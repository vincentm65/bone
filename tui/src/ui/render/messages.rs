//! Transcript message rendering (wrapping, markdown, and role styling).

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::chat::{Message, ToolDisplay};
use crate::llm::ChatRole;
use crate::ui::render::{markdown, wrap};
use crate::ui::theme::Theme;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

pub(crate) fn wrapped_line_count(line: &Line<'static>, width: u16) -> u16 {
    Paragraph::new(line.clone())
        .wrap(Wrap { trim: false })
        .line_count(width)
        .max(1) as u16
}

pub fn assistant_markdown_to_lines(content: &str, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    markdown::render_markdown(content, width, theme.code())
}

pub fn render_tool(
    tool: &ToolDisplay,
    content: &str,
    image_count: usize,
    theme: &Theme,
    lines: &mut Vec<Line<'static>>,
    width: usize,
    expanded: bool,
) {
    render_tool_with_hint(
        tool,
        content,
        image_count,
        theme,
        lines,
        width,
        expanded,
        true,
    );
}

pub(crate) fn render_tool_with_hint(
    tool: &ToolDisplay,
    content: &str,
    image_count: usize,
    theme: &Theme,
    lines: &mut Vec<Line<'static>>,
    width: usize,
    expanded: bool,
    show_expand_hint: bool,
) {
    let marker = if tool.is_error { "✕ " } else { "  " };
    let name_style = tool_name_style(tool, theme);
    let rest_style = tool_rest_style(tool, theme);
    let marker_style = Style::default().fg(theme.tool_error);
    let indent = "    ";
    let prefix_width = 4;
    let label_width = width.saturating_sub(prefix_width).max(1);

    if !tool.label.is_empty() {
        let wrapped = wrap_tool_label(&tool.label, label_width);
        let logical = tool.label.split('\n').collect::<Vec<_>>();
        let mut visual_idx = 0usize;
        let mut heredoc_delim: Option<String> = None;

        for (_line_idx, raw_line) in logical.iter().enumerate() {
            let in_heredoc_body = heredoc_delim.is_some();
            let visuals = wrap_label_line(raw_line, label_width);
            let is_first_logical = visual_idx == 0;
            let styled_visuals = if tool.is_shell {
                if in_heredoc_body {
                    visuals
                        .iter()
                        .map(|v| vec![Span::styled(v.clone(), rest_style)])
                        .collect()
                } else {
                    let rest = if is_first_logical {
                        raw_line.strip_prefix("shell").unwrap_or(raw_line)
                    } else {
                        raw_line
                    };
                    let tokens = lex_shell(rest);
                    let mut spans = shell_spans_from_tokens(&tokens, theme);
                    if is_first_logical {
                        spans.insert(
                            0,
                            Span::styled("shell".to_string(), Style::default().fg(Color::White)),
                        );
                    }
                    wrap_tool_label_spans(spans, &visuals)
                }
            } else {
                wrap_tool_label_spans(
                    tool_label_spans(raw_line, name_style, rest_style, theme),
                    &visuals,
                )
            };
            for (i, _) in visuals.into_iter().enumerate() {
                if visual_idx == 0 {
                    let mut spans = vec![Span::raw("  "), Span::styled(marker, marker_style)];
                    spans.extend(styled_visuals[i].clone());
                    lines.push(Line::from(spans));
                } else {
                    let mut spans = vec![Span::raw(indent)];
                    spans.extend(styled_visuals[i].clone());
                    lines.push(Line::from(spans));
                }
                visual_idx += 1;
            }
            if let Some(delim) = heredoc_delim.as_deref() {
                if raw_line.trim() == delim {
                    heredoc_delim = None;
                }
            } else if let Some(delim) = heredoc_delimiter(raw_line) {
                heredoc_delim = Some(delim);
            }
        }
        debug_assert_eq!(visual_idx, wrapped.len());
    }

    if tool.is_shell {
        render_shell_output(
            content,
            tool.is_error,
            expanded,
            show_expand_hint,
            theme,
            lines,
            width,
        );
    } else if !content.is_empty() {
        render_tool_content(content, theme, lines, width);
    }
    if image_count > 0 {
        for idx in 1..=image_count {
            let label = if image_count == 1 {
                "  image (PNG)".to_string()
            } else {
                format!("  image {idx} (PNG)")
            };
            lines.push(Line::from(Span::styled(
                label,
                Style::default().fg(theme.system_msg),
            )));
        }
    }
}

fn tool_name_style(_tool: &ToolDisplay, _theme: &Theme) -> Style {
    Style::default().fg(Color::White)
}

fn tool_rest_style(_tool: &ToolDisplay, theme: &Theme) -> Style {
    Style::default().fg(theme.tool_call)
}

/// First label line of a non-shell tool: tool name accented, and for the file
/// tools the path colored like shell paths with any trailing `(…)` summary
/// kept muted, mirroring the shell label highlighting.
fn tool_label_spans(
    line: &str,
    name_style: Style,
    rest_style: Style,
    theme: &Theme,
) -> Vec<Span<'static>> {
    let p = line.find(' ').unwrap_or(line.len());
    let (name, rest) = line.split_at(p);
    let mut spans = vec![Span::styled(name.to_string(), name_style)];
    if !matches!(name, "read_file" | "write_file" | "edit_file") {
        if !rest.is_empty() {
            spans.push(Span::styled(rest.to_string(), rest_style));
        }
        return spans;
    }

    let summary_idx = rest
        .rfind(" (")
        .filter(|_| rest.ends_with(')'))
        .unwrap_or(rest.len());
    let (path, summary) = rest.split_at(summary_idx);
    if !path.is_empty() {
        spans.push(Span::styled(
            path.to_string(),
            Style::default().fg(theme.shell_path),
        ));
    }
    if !summary.is_empty() {
        spans.push(Span::styled(summary.to_string(), rest_style));
    }
    spans
}

fn wrap_tool_label_spans(spans: Vec<Span<'static>>, visuals: &[String]) -> Vec<Vec<Span<'static>>> {
    let mut out = Vec::with_capacity(visuals.len());
    let mut span_idx = 0usize;
    let mut offset = 0usize;

    for visual in visuals {
        let mut needed = visual.len();
        let mut line = Vec::new();
        while needed > 0 && span_idx < spans.len() {
            let content = spans[span_idx].content.as_ref();
            let rest = &content[offset..];
            let take = needed.min(rest.len());
            if take > 0 {
                line.push(Span::styled(
                    rest[..take].to_string(),
                    spans[span_idx].style,
                ));
            }
            needed -= take;
            offset += take;
            if offset == content.len() {
                span_idx += 1;
                offset = 0;
            }
        }
        out.push(line);
    }

    out
}

fn render_tool_content(content: &str, theme: &Theme, lines: &mut Vec<Line<'static>>, width: usize) {
    for raw_line in content.lines() {
        for visual_line in wrap::wrap_text(raw_line, width) {
            let spans = style_tool_content_line(&visual_line, theme);
            lines.push(Line::from(spans));
        }
    }
}

fn style_tool_content_line(text: &str, theme: &Theme) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut current = String::new();
    let mut in_link = false;

    let parts = split_content_into_links_and_text(text);
    for part in parts {
        if in_link && !current.is_empty() {
            spans.push(Span::styled(
                std::mem::take(&mut current),
                Style::default().fg(theme.tool_call),
            ));
            current.clear();
            in_link = false;
        }
        match part {
            ContentPart::Text(s) => current.push_str(&s),
            ContentPart::Link(s) => {
                if !current.is_empty() {
                    spans.push(Span::styled(
                        std::mem::take(&mut current),
                        Style::default().fg(theme.tool_call),
                    ));
                }
                spans.push(Span::styled(s, Style::default().fg(theme.shell_path)));
                in_link = true;
            }
        }
    }
    if !current.is_empty() {
        spans.push(Span::styled(current, Style::default().fg(theme.tool_call)));
    }
    spans
}

enum ContentPart {
    Text(String),
    Link(String),
}

fn split_content_into_links_and_text(text: &str) -> Vec<ContentPart> {
    let mut parts = Vec::new();
    let mut start = 0;
    let bytes = text.as_bytes();
    let len = bytes.len();

    while start < len {
        if let Some((link_start, link_end)) = find_link_start(bytes, start) {
            if link_start > start {
                parts.push(ContentPart::Text(text[start..link_start].to_string()));
            }
            if let Some(end) = find_link_end(bytes, link_end) {
                parts.push(ContentPart::Link(text[link_start..end].to_string()));
                start = end;
            } else {
                start = len;
            }
        } else {
            parts.push(ContentPart::Text(text[start..].to_string()));
            break;
        }
    }

    if parts.is_empty() {
        parts.push(ContentPart::Text(text.to_string()));
    }
    parts
}

fn find_link_start(bytes: &[u8], start: usize) -> Option<(usize, usize)> {
    let len = bytes.len();
    if start >= len {
        return None;
    }

    // Check for URL schemes
    for scheme in ["http://", "https://", "file://"] {
        if let Some(rest) = bytes[start..].windows(scheme.len()).next() {
            if rest == scheme.as_bytes() {
                return Some((start, start + scheme.len()));
            }
        }
    }

    // Check for file paths
    let b = bytes[start];
    if b == b'/' || b == b'$' {
        return Some((start, start + 1));
    }
    if b == b'~' && start + 1 < len && bytes[start + 1] == b'/' {
        return Some((start, start + 2));
    }
    if b == b'.' && start + 1 < len && (bytes[start + 1] == b'/' || bytes[start + 1] == b'.') {
        return Some((start, start + 1));
    }

    None
}

fn find_link_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start;
    let len = bytes.len();

    while i < len {
        let b = bytes[i];
        // URL/path characters: alnum, ., -, _, ~, :, /, ?, #, [, ], @, !, $, &, ', (, ), *, +, ,, ;, =, %
        if b.is_ascii_alphanumeric()
            || b == b'.'
            || b == b'-'
            || b == b'_'
            || b == b'~'
            || b == b':'
            || b == b'/'
            || b == b'?'
            || b == b'#'
            || b == b'['
            || b == b']'
            || b == b'@'
            || b == b'!'
            || b == b'$'
            || b == b'&'
            || b == b'\''
            || b == b'('
            || b == b')'
            || b == b'*'
            || b == b'+'
            || b == b','
            || b == b';'
            || b == b'='
            || b == b'%'
        {
            i += 1;
        } else {
            break;
        }
    }

    if i > start { Some(i) } else { None }
}

fn render_shell_output(
    content: &str,
    is_error: bool,
    expanded: bool,
    show_expand_hint: bool,
    theme: &Theme,
    lines: &mut Vec<Line<'static>>,
    width: usize,
) {
    if content.trim().is_empty() {
        return;
    }

    let raw_lines = shell_output_lines(content);
    if raw_lines.is_empty() {
        return;
    }
    let gutter_color = if is_error {
        theme.tool_error
    } else {
        theme.shell_separator
    };
    let gutter_style = Style::default().fg(gutter_color);
    let text_style = Style::default().fg(theme.tool_call);
    let body_width = width.saturating_sub(8).max(1);

    let visual_lines = raw_lines
        .into_iter()
        .flat_map(|logical| {
            wrap::wrap_text(&logical, body_width)
                .into_iter()
                .enumerate()
                .map(|(idx, line)| {
                    if idx == 0 {
                        line
                    } else {
                        line.trim_start().to_string()
                    }
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

    let shown = if expanded || visual_lines.len() <= 5 {
        visual_lines
    } else {
        let hidden = visual_lines.len().saturating_sub(4);
        let noun = if hidden == 1 {
            "terminal line"
        } else {
            "terminal lines"
        };
        // Shell tool results are already capped by core; expanded means up to that retained output.
        let mut shown = visual_lines.iter().take(2).cloned().collect::<Vec<_>>();
        let hint = if show_expand_hint { " (ctrl+o)" } else { "" };
        shown.push(format!("⋮ +{hidden} {noun}{hint}"));
        shown.extend(
            visual_lines
                .iter()
                .skip(visual_lines.len().saturating_sub(2))
                .cloned(),
        );
        shown
    };

    let last = shown.len().saturating_sub(1);
    for (idx, visual_line) in shown.into_iter().enumerate() {
        let gutter = if idx == last {
            "      ╰ "
        } else {
            "      │ "
        };
        lines.push(Line::from(vec![
            Span::styled(gutter, gutter_style),
            Span::styled(visual_line, text_style),
        ]));
    }
}

fn shell_output_lines(content: &str) -> Vec<String> {
    let mut lines = content.lines();
    if matches!(lines.next(), Some(line) if line.starts_with("exit code: ")) {
        if matches!(lines.next(), Some("stdout:")) {
            let rest = lines.collect::<Vec<_>>();
            let (stdout, stderr) = match rest.iter().position(|line| *line == "stderr:") {
                Some(pos) => (&rest[..pos], &rest[pos + 1..]),
                None => (&rest[..], &[][..]),
            };
            // Empty stdout still occupies a line in the wire format; drop it so
            // stderr-only output doesn't render a leading blank gutter line.
            let mut out = stdout.iter().map(|s| s.to_string()).collect::<Vec<_>>();
            while out.last().is_some_and(|line| line.is_empty()) {
                out.pop();
            }
            out.extend(stderr.iter().map(|s| s.to_string()));
            while out.last().is_some_and(|line| line.is_empty()) {
                out.pop();
            }
            return out;
        }
    }

    content.lines().map(str::to_string).collect()
}

fn shell_spans_from_tokens(tokens: &[ShellToken], theme: &Theme) -> Vec<Span<'static>> {
    let mut expect_program = true;
    let mut expect_redirect_target = false;
    let mut spans = Vec::new();
    for token in tokens {
        let color = match token.kind {
            ShellTokenKind::Space => theme.tool_call,
            ShellTokenKind::Separator => {
                expect_program = true;
                expect_redirect_target = false;
                theme.shell_separator
            }
            ShellTokenKind::Redirect => {
                expect_redirect_target = true;
                theme.shell_redirect
            }
            ShellTokenKind::String => {
                expect_program = false;
                expect_redirect_target = false;
                theme.shell_string
            }
            ShellTokenKind::Variable => {
                if !is_assignment(token.text) {
                    expect_program = false;
                }
                expect_redirect_target = false;
                theme.shell_variable
            }
            ShellTokenKind::Comment => theme.shell_comment,
            ShellTokenKind::Word => {
                if is_assignment(token.text) {
                    theme.shell_variable
                } else if expect_redirect_target {
                    expect_program = false;
                    expect_redirect_target = false;
                    theme.shell_path
                } else if expect_program {
                    expect_program = false;
                    theme.shell_program
                } else if is_flag(token.text) {
                    theme.shell_flag
                } else if is_path_like(token.text) {
                    theme.shell_path
                } else {
                    theme.tool_call
                }
            }
        };
        spans.push(Span::styled(
            token.text.to_string(),
            Style::default().fg(color),
        ));
    }
    spans
}

#[derive(Clone, Copy)]
enum ShellTokenKind {
    Space,
    Word,
    Separator,
    Redirect,
    String,
    Variable,
    Comment,
}

struct ShellToken<'a> {
    text: &'a str,
    kind: ShellTokenKind,
}

fn lex_shell(line: &str) -> Vec<ShellToken<'_>> {
    let mut out = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        let b = bytes[i];
        if b.is_ascii_whitespace() {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            out.push(ShellToken {
                text: &line[start..i],
                kind: ShellTokenKind::Space,
            });
        } else if b == b'#' && at_comment_start(bytes, i) {
            out.push(ShellToken {
                text: &line[start..],
                kind: ShellTokenKind::Comment,
            });
            break;
        } else if b == b'\'' || b == b'"' {
            i = quoted_end(bytes, i, b);
            out.push(ShellToken {
                text: &line[start..i],
                kind: ShellTokenKind::String,
            });
        } else if b == b'$' {
            i = variable_end(bytes, i);
            out.push(ShellToken {
                text: &line[start..i],
                kind: ShellTokenKind::Variable,
            });
        } else if let Some(end) = operator_end(line, i) {
            let kind = if is_redirect_operator(&line[i..end]) {
                ShellTokenKind::Redirect
            } else {
                ShellTokenKind::Separator
            };
            i = end;
            out.push(ShellToken {
                text: &line[start..i],
                kind,
            });
        } else {
            // Advance whole UTF-8 chars: `operator_end` slices `line` at `i`,
            // which panics off a char boundary.
            i += utf8_char_len(bytes[i]);
            while i < bytes.len()
                && !bytes[i].is_ascii_whitespace()
                && bytes[i] != b'\''
                && bytes[i] != b'"'
                && bytes[i] != b'$'
                && !at_comment_start(bytes, i)
                && operator_end(line, i).is_none()
            {
                i += utf8_char_len(bytes[i]);
            }
            out.push(ShellToken {
                text: &line[start..i],
                kind: ShellTokenKind::Word,
            });
        }
    }
    out
}

fn utf8_char_len(lead: u8) -> usize {
    match lead {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1,
    }
}

fn quoted_end(bytes: &[u8], start: usize, quote: u8) -> usize {
    let mut i = start + 1;
    let mut escaped = false;
    while i < bytes.len() {
        if escaped {
            escaped = false;
        } else if bytes[i] == b'\\' && quote == b'"' {
            escaped = true;
        } else if bytes[i] == quote {
            i += 1;
            break;
        }
        i += 1;
    }
    i
}

fn variable_end(bytes: &[u8], start: usize) -> usize {
    let mut i = start + 1;
    if i < bytes.len() && bytes[i] == b'{' {
        i += 1;
        while i < bytes.len() && bytes[i] != b'}' {
            i += 1;
        }
        return (i + 1).min(bytes.len());
    }
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
        i += 1;
    }
    if i == start + 1 { start + 1 } else { i }
}

fn operator_end(line: &str, start: usize) -> Option<usize> {
    let rest = &line[start..];
    for op in [
        "2>&1", "&>>", "2>>", "<<<", "&&", "||", ">>", "<<", "2>", "&>",
    ] {
        if rest.starts_with(op) {
            return Some(start + op.len());
        }
    }
    if rest.as_bytes().first().is_some_and(|b| b.is_ascii_digit()) {
        let mut i = start;
        while i < line.len() && line.as_bytes()[i].is_ascii_digit() {
            i += 1;
        }
        if line[i..].starts_with(">>") {
            return Some(i + 2);
        }
        if line[i..].starts_with('>') {
            return Some(i + 1);
        }
    }
    match rest.as_bytes().first().copied() {
        Some(b'|' | b';' | b'&' | b'>' | b'<' | b'(' | b')' | b'{' | b'}') => Some(start + 1),
        _ => None,
    }
}

fn is_redirect_operator(token: &str) -> bool {
    token.contains('>') || token.contains('<')
}

fn at_comment_start(bytes: &[u8], i: usize) -> bool {
    bytes[i] == b'#' && (i == 0 || bytes[i - 1].is_ascii_whitespace())
}

fn is_assignment(token: &str) -> bool {
    let Some(eq) = token.find('=') else {
        return false;
    };
    let name = &token[..eq];
    !name.is_empty()
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        && !name.as_bytes()[0].is_ascii_digit()
}

fn is_flag(token: &str) -> bool {
    token.starts_with('-') && token != "-" && token != "--"
}

fn is_path_like(token: &str) -> bool {
    token.starts_with('/')
        || token.starts_with("./")
        || token.starts_with("../")
        || token.starts_with("~/")
        || token.contains('/')
        || token.rsplit_once('.').is_some_and(|(_, ext)| {
            (1..=6).contains(&ext.len()) && ext.bytes().all(|b| b.is_ascii_alphanumeric())
        })
}

fn heredoc_delimiter(line: &str) -> Option<String> {
    let mut parts = line.split_whitespace();
    while let Some(part) = parts.next() {
        if part == "<<" || part == "<<-" {
            return parts.next().map(clean_heredoc_delim);
        }
        if let Some(rest) = part.strip_prefix("<<-").or_else(|| part.strip_prefix("<<"))
            && !rest.is_empty()
        {
            return Some(clean_heredoc_delim(rest));
        }
    }
    None
}

fn clean_heredoc_delim(s: &str) -> String {
    let trimmed = s.trim_matches(|c| c == '\'' || c == '"');
    if trimmed.starts_with("EOF") && trimmed.len() > 3 {
        "EOF".to_string()
    } else {
        trimmed.to_string()
    }
}

fn wrap_tool_label(label: &str, label_width: usize) -> Vec<String> {
    label
        .split('\n')
        .flat_map(|raw_line| wrap_label_line(raw_line, label_width))
        .collect::<Vec<_>>()
}

fn wrap_label_line(line: &str, width: usize) -> Vec<String> {
    let leading = line.len() - line.trim_start().len();
    if leading == 0 || leading >= line.len() {
        return wrap::wrap_text(line, width);
    }

    let first_prefix = &line[..leading];
    wrap::wrap_text_with_prefix(line.trim_start(), first_prefix, "", width)
}

fn render_diff_preview(
    content: &str,
    theme: &Theme,
    lines: &mut Vec<Line<'static>>,
    terminal_width: usize,
) {
    let content = content.strip_prefix('\n').unwrap_or(content);
    let indent = "  ";
    let effective_width = terminal_width.saturating_sub(indent.len());
    for raw_line in content.lines() {
        let (visual_lines, style, fill_background) =
            if let Some((gutter, body, marker)) = numbered_diff_parts(raw_line) {
                let style = match marker {
                    '-' => Style::default().bg(theme.diff_removed),
                    '+' => Style::default().bg(theme.diff_added),
                    _ => Style::default().fg(theme.tool_call),
                };
                (
                    wrap_numbered_diff_line(gutter, body, effective_width),
                    style,
                    matches!(marker, '-' | '+'),
                )
            } else {
                (
                    wrap::wrap_text(raw_line, effective_width),
                    Style::default().fg(theme.system_msg),
                    false,
                )
            };

        for visual_line in visual_lines {
            let prefixed = format!("{indent}{visual_line}");
            let line = if fill_background {
                pad_to_terminal_width(&prefixed, terminal_width)
            } else {
                prefixed
            };
            if let Some(spans) = header_spans_for_line(&line, theme) {
                lines.push(Line::from(spans));
            } else {
                lines.push(Line::from(Span::styled(line, style)));
            }
        }
    }
}

fn header_spans_for_line(line: &str, theme: &Theme) -> Option<Vec<Span<'static>>> {
    // Check if this line is part of a diff header (first line or continuation)
    let trimmed = line.trim_start();
    if trimmed.is_empty() {
        return None;
    }
    let name_end = trimmed.find(' ')?;
    let tool_name = &trimmed[..name_end];
    let rest = &trimmed[name_end + 1..];
    let paren_start = rest.rfind(" (-")?;
    let counts = &rest[paren_start + 1..];
    if !counts.ends_with(')') || !counts.contains(" | +") {
        return None;
    }

    // If line is longer than the header, it's a wrapped continuation
    let full_header = format!("{}{counts}", &rest[..paren_start]);
    if line.len() > trimmed.len() + (full_header.len() - name_end - 1) {
        // Wrapped continuation — use full line with detail style
        return Some(vec![Span::styled(
            line.to_string(),
            Style::default().fg(theme.tool_call),
        )]);
    }

    let path = &rest[..paren_start];
    let name_style = Style::default().fg(Color::White);
    let detail_style = Style::default().fg(theme.tool_call);

    Some(vec![
        Span::raw("  "),
        Span::styled("  ", Style::default().fg(theme.tool_error)),
        Span::styled(tool_name.to_string(), name_style),
        Span::styled(format!(" {path}"), Style::default().fg(theme.shell_path)),
        Span::styled(format!(" {counts}"), detail_style),
    ])
}

fn numbered_diff_parts(line: &str) -> Option<(&str, &str, char)> {
    let gutter = line.get(..8)?;
    let body = line.get(8..)?;
    let marker = *gutter.as_bytes().get(6)? as char;
    let has_number = gutter.get(..5)?.trim().parse::<usize>().is_ok();

    (has_number && matches!(marker, ' ' | '+' | '-')).then_some((gutter, body, marker))
}

fn wrap_numbered_diff_line(gutter: &str, body: &str, terminal_width: usize) -> Vec<String> {
    let indent_end = body.len() - body.trim_start().len();
    let indent = &body[..indent_end];
    let first_prefix = format!("{gutter}{indent}");
    let continuation_prefix = format!("        {indent}");

    wrap::wrap_text_with_prefix(
        body.trim_start(),
        &first_prefix,
        &continuation_prefix,
        terminal_width,
    )
}

fn pad_to_terminal_width(line: &str, terminal_width: usize) -> String {
    let terminal_width = terminal_width.max(1);
    let width = UnicodeWidthStr::width(line);
    // Pad with spaces to fill terminal width for full-width background coloring
    let pad = terminal_width.saturating_sub(width);
    format!("{line}{}", " ".repeat(pad))
}

fn truncate_to_display_width(text: &str, max_width: usize) -> String {
    let mut fitted = String::new();
    let mut used = 0;

    for ch in text.chars() {
        let width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + width > max_width {
            break;
        }
        fitted.push(ch);
        used += width;
    }

    fitted
}

fn wrap_user_line(raw_line: &str, first_line: bool, width: usize) -> Vec<String> {
    // Avoid writing styled user rows through the terminal's final column when
    // inserting into native scrollback; terminals may auto-wrap that cell.
    let width = width.saturating_sub(1).max(1);
    let leading = raw_line.len() - raw_line.trim_start().len();
    let indent = &raw_line[..leading];
    let content = &raw_line[leading..];
    let marker = if first_line { "> " } else { "" };
    let required_content_width = usize::from(!content.is_empty());
    let prefix_limit = width.saturating_sub(required_content_width);
    let first_prefix = truncate_to_display_width(&format!("{marker}{indent}"), prefix_limit);
    let rest_prefix = truncate_to_display_width(indent, prefix_limit);

    wrap::wrap_text_with_prefix(content, &first_prefix, &rest_prefix, width)
}

fn render_content(msg: &Message, theme: &Theme, lines: &mut Vec<Line<'static>>, width: u16) {
    if matches!(msg.role, ChatRole::System) && msg.content.starts_with("\n") {
        render_diff_preview(&msg.content, theme, lines, width as usize);
        return;
    }

    match msg.role {
        ChatRole::User => {
            for (idx, raw_line) in msg.content.lines().enumerate() {
                for visual_line in wrap_user_line(raw_line, idx == 0, width as usize) {
                    lines.push(Line::from(Span::styled(
                        visual_line,
                        Style::default().fg(theme.user_msg).bg(theme.user_msg_bg),
                    )));
                }
            }
            for idx in 1..=msg.image_count {
                let label = if msg.image_count == 1 {
                    "image (PNG)".to_string()
                } else {
                    format!("image {idx} (PNG)")
                };
                for visual_line in
                    wrap_user_line(&label, msg.content.is_empty() && idx == 1, width as usize)
                {
                    lines.push(Line::from(Span::styled(
                        visual_line,
                        Style::default().fg(theme.user_msg).bg(theme.user_msg_bg),
                    )));
                }
            }
        }
        ChatRole::Assistant => {
            let rendered = markdown::render_markdown(&msg.content, width, theme.code());
            lines.extend(rendered);
        }
        ChatRole::System | ChatRole::Tool => {
            for raw_line in msg.content.lines() {
                for visual_line in wrap::wrap_text(raw_line, width as usize) {
                    lines.push(Line::from(Span::styled(
                        visual_line,
                        Style::default().fg(theme.system_msg),
                    )));
                }
            }
        }
    }
}

fn role_changed(prev_role: Option<ChatRole>, current_role: ChatRole) -> bool {
    matches!(
        (prev_role.unwrap_or(current_role), current_role),
        (
            ChatRole::User,
            ChatRole::Assistant | ChatRole::System | ChatRole::Tool
        ) | (
            ChatRole::Assistant | ChatRole::System | ChatRole::Tool,
            ChatRole::User
        )
    )
}

pub fn msg_to_lines(
    msgs: &[Message],
    theme: &Theme,
    prev_role: Option<ChatRole>,
    width: u16,
    expanded: bool,
) -> Vec<Line<'static>> {
    msg_to_lines_with_shell_hint(msgs, theme, prev_role, width, expanded, true)
}

pub(crate) fn msg_to_lines_with_shell_hint(
    msgs: &[Message],
    theme: &Theme,
    prev_role: Option<ChatRole>,
    width: u16,
    expanded: bool,
    show_expand_hint: bool,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut prev_role = prev_role;

    for msg in msgs {
        // Skip invisible placeholders (e.g., empty assistant messages between
        // tool rounds) so they don't inject extra blank-line gaps.
        if msg.tool.is_none() && msg.content.is_empty() && msg.image_count == 0 {
            prev_role = Some(msg.role);
            continue;
        }

        if role_changed(prev_role, msg.role) {
            lines.push(Line::raw(""));
        }

        if let Some(tool) = &msg.tool {
            render_tool_with_hint(
                tool,
                &msg.content,
                msg.image_count,
                theme,
                &mut lines,
                width as usize,
                expanded,
                show_expand_hint,
            );
        } else {
            render_content(msg, theme, &mut lines, width);
        }

        prev_role = Some(msg.role);
        let suppress_trailing_blank = msg
            .tool
            .as_ref()
            .is_some_and(|tool| tool.is_shell && !tool.label.is_empty() && msg.content.is_empty());
        if !suppress_trailing_blank {
            lines.push(Line::raw(""));
        }
    }

    if lines.is_empty() {
        lines.push(Line::raw(""));
    }

    lines
}

use crate::chat::Message;
use crate::tools::types::{ToolCall, ToolResult};

pub fn build_tool_row(call: &ToolCall, result: &ToolResult) -> Message {
    Message::tool_row(tool_label(call, result), result.is_error)
}

pub fn tool_label(call: &ToolCall, result: &ToolResult) -> String {
    if call.name == "shell" {
        return call
            .arguments
            .get("command")
            .and_then(|value| value.as_str())
            .map(format_shell_label)
            .unwrap_or_else(|| call.name.clone());
    }

    let target = call
        .arguments
        .get("path")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| call.arguments.get("query").and_then(|v| v.as_str()));

    let mut label = match target {
        Some(target) => format!("{} {}", call.name, target),
        None => call.name.clone(),
    };



    if call.name == "read_file" && !result.is_error {
        label.push_str(&read_file_line_summary(call, result));
    }

    label
}

pub fn read_file_line_summary(call: &ToolCall, result: &ToolResult) -> String {
    let lines_read = result.content.lines().count();
    if lines_read == 0 {
        return " (0 lines)".to_string();
    }

    let start_line = call.arguments["start_line"].as_u64().unwrap_or(1) as usize;
    let end_line = start_line + lines_read - 1;
    format!(" (lines {start_line}-{end_line}, {lines_read} read)")
}

pub fn format_shell_label(command: &str) -> String {
    let mut command_lines = format_shell_command(command).into_iter();
    let mut lines = vec![match command_lines.next() {
        Some(line) => format!("shell {line}"),
        None => "shell".to_string(),
    }];
    for line in command_lines {
        lines.push(format!(" {line}"));
    }
    lines.join("\n")
}

pub(crate) fn format_shell_command(command: &str) -> Vec<String> {
    if find_heredoc_marker(command).is_some() {
        return expand_collapsed_heredoc_line(command);
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut chars = command.chars().peekable();
    let mut single = false;
    let mut double = false;
    let mut escaped = false;

    while let Some(ch) = chars.next() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }

        if ch == '\\' {
            current.push(ch);
            escaped = true;
            continue;
        }

        if ch == '\'' && !double {
            single = !single;
            current.push(ch);
            continue;
        }

        if ch == '"' && !single {
            double = !double;
            current.push(ch);
            continue;
        }

        if !single && !double {
            match ch {
                '&' if chars.peek() == Some(&'&') => {
                    current.push_str("&&");
                    chars.next();
                    push_shell_line(&mut lines, &mut current);
                    continue;
                }
                '|' if chars.peek() == Some(&'|') => {
                    current.push_str("||");
                    chars.next();
                    push_shell_line(&mut lines, &mut current);
                    continue;
                }
                '|' | ';' => {
                    current.push(ch);
                    push_shell_line(&mut lines, &mut current);
                    continue;
                }
                _ => {}
            }
        }

        current.push(ch);
    }

    push_shell_line(&mut lines, &mut current);
    lines
}

fn push_shell_line(lines: &mut Vec<String>, current: &mut String) {
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        lines.push(trimmed.to_string());
    }
    current.clear();
}

fn expand_collapsed_heredoc_line(line: &str) -> Vec<String> {
    let Some(marker) = find_heredoc_marker(line) else {
        return vec![line.to_string()];
    };
    let Some(body_start) = line[marker.after_start..].find(&marker.delimiter) else {
        return vec![line.to_string()];
    };

    let delimiter_start = marker.after_start + body_start;
    let body = line[marker.after_start..delimiter_start].trim();
    let rest_start = delimiter_start + marker.delimiter.len();
    let rest = line[rest_start..].trim();

    let mut out = vec![line[..marker.after_start].trim_end().to_string()];
    for payload_line in reflow_code_payload(body) {
        out.push(format!(" {payload_line}"));
    }
    out.push(marker.delimiter);
    if !rest.is_empty() {
        out.extend(format_shell_command(rest));
    }
    out
}

struct HeredocMarker {
    delimiter: String,
    after_start: usize,
}

fn find_heredoc_marker(line: &str) -> Option<HeredocMarker> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'<' && bytes[i + 1] == b'<' {
            i += 2;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'-' {
                i += 1;
                while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
            }

            let (delimiter, after) = read_heredoc_delimiter(line, i)?;
            return Some(HeredocMarker {
                delimiter,
                after_start: after,
            });
        }
        i += 1;
    }
    None
}

fn read_heredoc_delimiter(line: &str, start: usize) -> Option<(String, usize)> {
    let bytes = line.as_bytes();
    let quote = bytes
        .get(start)
        .copied()
        .filter(|b| *b == b'\'' || *b == b'"');
    if let Some(quote) = quote {
        let mut end = start + 1;
        while end < bytes.len() && bytes[end] != quote {
            end += 1;
        }
        if end >= bytes.len() {
            return None;
        }
        return Some((line[start + 1..end].to_string(), end + 1));
    }

    let mut end = start;
    while end < bytes.len() && !bytes[end].is_ascii_whitespace() {
        end += 1;
    }
    if line[start..end].starts_with("EOF") && line[start..end].len() > 3 {
        return Some(("EOF".to_string(), start + 3));
    }
    (end > start).then(|| (line[start..end].to_string(), end))
}

fn reflow_code_payload(payload: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut indent = 0usize;
    let mut chars = payload.chars().peekable();
    let mut single = false;
    let mut double = false;
    let mut escaped = false;

    while let Some(ch) = chars.next() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' {
            current.push(ch);
            escaped = true;
            continue;
        }
        if ch == '\'' && !double {
            single = !single;
            current.push(ch);
            continue;
        }
        if ch == '"' && !single {
            double = !double;
            current.push(ch);
            continue;
        }

        if !single && !double && ch == '/' && chars.peek() == Some(&'/') {
            flush_code_line(&mut lines, &mut current, indent);
            current.push_str("//");
            chars.next();
            continue;
        }

        if !single && !double && ch == '{' {
            flush_code_line(&mut lines, &mut current, indent);
            current.push(ch);
            flush_code_line(&mut lines, &mut current, indent);
            indent += 1;
            continue;
        }

        if !single && !double && ch == '}' {
            flush_code_line(&mut lines, &mut current, indent);
            indent = indent.saturating_sub(1);
            current.push(ch);
            flush_code_line(&mut lines, &mut current, indent);
            continue;
        }

        if !single && !double && ch == ';' {
            current.push(ch);
            flush_code_line(&mut lines, &mut current, indent);
            continue;
        }

        current.push(ch);
    }

    flush_code_line(&mut lines, &mut current, indent);
    if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    }
}

fn flush_code_line(lines: &mut Vec<String>, current: &mut String, indent: usize) {
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        lines.push(format!("{}{}", "  ".repeat(indent), trimmed));
    }
    current.clear();
}

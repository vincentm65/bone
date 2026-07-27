//! Builds the tool-call summary rows shown in the transcript.

use crate::chat::Message;
use crate::llm::ChatRole;
use crate::tools::types::{ToolCall, ToolDisplayConfig, ToolResult};
use serde_json::Value;

pub fn build_tool_row(
    call: &ToolCall,
    result: &ToolResult,
    display: Option<&ToolDisplayConfig>,
) -> Message {
    let show_label = display.and_then(|d| d.show).unwrap_or(true);
    let is_shell = call.name == "shell";
    let show_result = display.and_then(|d| d.show_result).unwrap_or(false);
    let firefox_failed = firefox_failure_message(call, result).is_some();
    let label = if show_label {
        tool_label(call, result, display)
    } else {
        String::new()
    };
    // ShellTool caps stdout/stderr before returning; retained shell content is
    // the full post-cap output used by the expanded transcript viewer.
    let content = if is_shell || show_result {
        result.content.clone()
    } else {
        String::new()
    };
    Message {
        role: ChatRole::Tool,
        content,
        tool: Some(crate::chat::ToolDisplay {
            label,
            is_error: result.is_error || firefox_failed,
            is_shell,
        }),
        image_count: result.images.len(),
    }
}

pub fn shell_row(cmd: &str, output: String, is_error: bool) -> Message {
    Message {
        role: ChatRole::Tool,
        content: output,
        tool: Some(crate::chat::ToolDisplay {
            label: format_shell_label(cmd),
            is_error,
            is_shell: true,
        }),
        image_count: 0,
    }
}

pub fn tool_label(
    call: &ToolCall,
    result: &ToolResult,
    display: Option<&ToolDisplayConfig>,
) -> String {
    if call.name == "shell" {
        return format_shell_call_label(&call.arguments);
    }

    if let Some(label) = format_firefox_label(call, result) {
        return label;
    }

    if let Some(display_label) = display.and_then(|display| format_display_label(call, display)) {
        return display_label;
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

fn format_firefox_label(call: &ToolCall, result: &ToolResult) -> Option<String> {
    let action = firefox_action(call)?;
    let tool = call.name.strip_suffix("_observe").unwrap_or(&call.name);
    let prefix = format!("{tool} {action}");

    if let Some(error) = firefox_failure_message(call, result) {
        return Some(if error.is_empty() {
            format!("{prefix} — failed")
        } else {
            format!("{prefix} — failed: {error}")
        });
    }

    let response = serde_json::from_str::<Value>(result.content.trim()).ok();
    let response = response.as_ref();
    let description = match action {
        "observe" => firefox_page_description(response)
            .map(|page| format!("observed {page}"))
            .unwrap_or_else(|| "observed page".to_string()),
        "navigate" => firefox_string(response, &[&["url"], &["current_url"], &["tab", "url"]])
            .or_else(|| argument_string(&call.arguments, &["url"]))
            .map(|url| format!("opened {}", concise_url(&url)))
            .unwrap_or_else(|| "navigation completed".to_string()),
        "click" => {
            if let Some(name) = firefox_element_name(response, &call.arguments) {
                format!("clicked “{}”", clean_label_text(&name, 80))
            } else if let Some(reference) = firefox_string(response, &[&["clicked"]])
                .or_else(|| argument_string(&call.arguments, &["ref"]))
            {
                format!("clicked ref {}", clean_label_text(&reference, 80))
            } else {
                "click completed".to_string()
            }
        }
        "type" => {
            if let Some(name) = firefox_element_name(response, &call.arguments) {
                format!("typed into “{}”", clean_label_text(&name, 80))
            } else if let Some(reference) = firefox_string(response, &[&["typed"]])
                .or_else(|| argument_string(&call.arguments, &["ref"]))
            {
                format!("typed into ref {}", clean_label_text(&reference, 80))
            } else {
                "typing completed".to_string()
            }
        }
        "press" => firefox_string(response, &[&["pressed"]])
            .or_else(|| argument_string(&call.arguments, &["key"]))
            .map(|key| format!("pressed {}", clean_label_text(&key, 80)))
            .unwrap_or_else(|| "key press completed".to_string()),
        "scroll" => {
            let x = response
                .and_then(|value| value.get("x"))
                .and_then(Value::as_i64);
            let y = response
                .and_then(|value| value.get("y"))
                .and_then(Value::as_i64);
            match (x, y) {
                (Some(x), Some(y)) => format!("scrolled to {x}, {y}"),
                _ => "scroll completed".to_string(),
            }
        }
        _ => "completed".to_string(),
    };

    Some(format!("{prefix} — {}", truncate_label(&description, 120)))
}

fn firefox_action(call: &ToolCall) -> Option<&str> {
    match call.name.as_str() {
        "firefox" | "browser_bridge" => call.arguments.get("action").and_then(Value::as_str),
        "firefox_observe" | "browser_bridge_observe" => Some("observe"),
        _ => None,
    }
}

fn firefox_failure_message(call: &ToolCall, result: &ToolResult) -> Option<String> {
    firefox_action(call)?;
    let response = serde_json::from_str::<Value>(result.content.trim()).ok();
    let structured_error = response
        .as_ref()
        .filter(|value| value.get("ok").and_then(Value::as_bool) == Some(false));

    if !result.is_error && structured_error.is_none() {
        return None;
    }

    let error = response
        .as_ref()
        .and_then(|value| {
            firefox_string(
                Some(value),
                &[&["message"], &["error"], &["stderr"], &["content"]],
            )
        })
        .unwrap_or_else(|| result.content.clone());
    Some(clean_label_text(&error, 120))
}

fn firefox_page_description(response: Option<&Value>) -> Option<String> {
    firefox_string(
        response,
        &[
            &["title"],
            &["tab", "title"],
            &["page", "title"],
            &["url"],
            &["current_url"],
            &["tab", "url"],
        ],
    )
    .map(|value| {
        if value.contains("://") {
            concise_url(&value)
        } else {
            clean_label_text(&value, 100)
        }
    })
}

fn firefox_element_name(response: Option<&Value>, arguments: &Value) -> Option<String> {
    firefox_string(
        response,
        &[
            &["label"],
            &["name"],
            &["text"],
            &["accessible_name"],
            &["element", "label"],
            &["element", "name"],
            &["element", "text"],
        ],
    )
    .or_else(|| argument_string(arguments, &["label", "name", "text", "accessible_name"]))
}

fn firefox_string(value: Option<&Value>, paths: &[&[&str]]) -> Option<String> {
    paths.iter().find_map(|path| {
        let value = path.iter().try_fold(value?, |value, key| value.get(*key))?;
        value
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

fn argument_string(arguments: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        arguments
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

fn concise_url(url: &str) -> String {
    let cleaned = clean_label_text(url, 100);
    let without_scheme = cleaned
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(&cleaned);
    let authority = without_scheme.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit('@').next().unwrap_or(authority);
    let host = host.split(':').next().unwrap_or(host).trim_end_matches('.');
    if host.is_empty() {
        cleaned
    } else {
        host.to_string()
    }
}

fn clean_label_text(text: &str, max: usize) -> String {
    let cleaned = text.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_label(&cleaned, max)
}

/// Truncate to `max` chars on a char boundary, appending an ellipsis.
fn truncate_label(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let kept: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}…")
}

fn format_display_label(call: &ToolCall, display: &ToolDisplayConfig) -> Option<String> {
    if let Some(template) = display.template.as_deref()
        && let Some(rendered) = render_display_template(template, &call.arguments)
        && !rendered.trim().is_empty()
    {
        return Some(format!("{} {}", call.name, rendered.trim()));
    }

    let parts = display
        .args
        .iter()
        .filter_map(|arg| {
            call.arguments
                .get(arg)
                .filter(|value| !value.is_null())
                .map(|value| format!("{arg}={}", format_display_value(value)))
        })
        .collect::<Vec<_>>();

    if parts.is_empty() {
        None
    } else {
        Some(format!("{} {}", call.name, parts.join(" ")))
    }
}

/// Render a display template. Scalar placeholders `{key}` interpolate argument
/// values; an array placeholder `{name[].f1|f2}` expands to the first present
/// field (`f1` then `f2`…) of each element of array arg `name`, each value
/// cleaned/truncated/quoted and joined with `, `.
///
/// Returns `None` when the template contains an array placeholder that resolves
/// to nothing (array arg absent or empty) — this lets a "list" template apply
/// only when the list is present (e.g. a dispatch label that shouldn't render
/// for non-dispatch actions), falling back to the `args` label instead.
fn render_display_template(template: &str, arguments: &Value) -> Option<String> {
    let map = arguments.as_object();
    let mut out = String::new();
    let mut rest = template;
    let mut empty_array_placeholder = false;

    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            // Unterminated placeholder — emit the rest verbatim.
            out.push('{');
            rest = after;
            continue;
        };
        let key = &after[..end];
        rest = &after[end + 1..];

        if let Some(idx) = key.find("[].") {
            let arr_name = &key[..idx];
            let fields: Vec<&str> = key[idx + 3..].split('|').collect();
            let items: Vec<String> = map
                .and_then(|m| m.get(arr_name))
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|el| pick_field(el, &fields))
                        .collect()
                })
                .unwrap_or_default();
            if items.is_empty() {
                empty_array_placeholder = true;
            }
            out.push_str(&items.join(", "));
        } else {
            let value = map.and_then(|m| m.get(key));
            out.push_str(&value.map(format_display_value).unwrap_or_default());
        }
    }
    out.push_str(rest);

    if empty_array_placeholder {
        return None;
    }
    Some(out)
}

/// First non-empty `fields` entry on `el`, cleaned of newlines, truncated, and
/// quoted — the per-element rendering for an array template placeholder.
fn pick_field(el: &Value, fields: &[&str]) -> Option<String> {
    for field in fields {
        if let Some(s) = el.get(field).and_then(|v| v.as_str()) {
            let s = s.trim();
            if !s.is_empty() {
                let cleaned = s.replace(['\n', '\r'], " ");
                return Some(format!("\"{}\"", truncate_label(&cleaned, 60)));
            }
        }
    }
    None
}

fn format_display_value(value: &Value) -> String {
    match value {
        Value::String(value) => {
            if value.chars().any(char::is_whitespace) {
                format!("\"{value}\"")
            } else {
                value.clone()
            }
        }
        Value::Array(values) => {
            let rendered = values
                .iter()
                .map(format_display_value)
                .collect::<Vec<_>>()
                .join(", ");
            format!("[{rendered}]")
        }
        Value::Object(_) => value.to_string(),
        Value::Null => String::new(),
        Value::Bool(_) | Value::Number(_) => value.to_string(),
    }
}

pub fn read_file_line_summary(call: &ToolCall, result: &ToolResult) -> String {
    // Current read_file output has two metadata lines followed by `N | text`.
    // Count only numbered content rows, not the File/Range/Note headers.
    let numbered_lines = result
        .content
        .lines()
        .filter(|line| {
            line.split_once(" | ")
                .is_some_and(|(prefix, _)| prefix.trim().parse::<usize>().is_ok())
        })
        .count();
    if result.content.starts_with("File: ") {
        if numbered_lines == 0 {
            return " (0 lines)".to_string();
        }
        let start_line = call.arguments["start_line"].as_u64().unwrap_or(1) as usize;
        let end_line = start_line + numbered_lines - 1;
        return format!(" (lines {start_line}-{end_line}, {numbered_lines} read)");
    }

    // The result ends with a bracketed status footer ("\n\n[...]") that is
    // not file content; don't count it toward lines read.
    let content = result
        .content
        .rsplit_once("\n\n[")
        .filter(|(_, tail)| tail.ends_with(']') && !tail.contains('\n'))
        .map(|(body, _)| body)
        .unwrap_or(&result.content);
    let lines_read =
        if content.starts_with('[') && content.ends_with(']') && !content.contains('\n') {
            0
        } else {
            content.lines().count()
        };
    if lines_read == 0 {
        return " (0 lines)".to_string();
    }

    let start_line = call.arguments["start_line"].as_u64().unwrap_or(1) as usize;
    let end_line = start_line + lines_read - 1;
    format!(" (lines {start_line}-{end_line}, {lines_read} read)")
}

pub fn format_shell_call_label(arguments: &Value) -> String {
    let action = arguments
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("run");
    if action == "run" {
        return arguments
            .get("command")
            .and_then(Value::as_str)
            .map(format_shell_label)
            .unwrap_or_else(|| "shell".to_string());
    }

    match arguments.get("id").and_then(Value::as_str) {
        Some(id) => format!("shell {action} {id}"),
        None => format!("shell {action}"),
    }
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
    command
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .map(str::to_string)
        .collect()
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

#[cfg(test)]
mod tests {
    use super::{build_tool_row, tool_label};
    use crate::tools::types::{ToolCall, ToolResult};
    use serde_json::json;

    fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "call-1".into(),
            name: name.into(),
            arguments,
        }
    }

    fn result(content: &str) -> ToolResult {
        ToolResult {
            call_id: "call-1".into(),
            name: "firefox".into(),
            content: content.into(),
            ..Default::default()
        }
    }

    #[test]
    fn summarizes_firefox_observation_from_returned_title() {
        let call = call("firefox", json!({"action":"observe"}));
        let result = result(r#"{"title":"Amazon product page","url":"https://amazon.com/item"}"#);

        assert_eq!(
            tool_label(&call, &result, None),
            "firefox observe — observed Amazon product page"
        );
    }

    #[test]
    fn browser_bridge_labels_include_the_action() {
        let click = call("browser_bridge", json!({"action":"click"}));
        assert_eq!(
            tool_label(&click, &result("{}"), None),
            "browser_bridge click — click completed"
        );

        let observe = call("browser_bridge_observe", json!({}));
        assert_eq!(
            tool_label(&observe, &result("{}"), None),
            "browser_bridge observe — observed page"
        );
    }

    #[test]
    fn every_firefox_label_includes_its_action() {
        for action in [
            "observe",
            "click",
            "type",
            "press",
            "scroll",
            "navigate",
            "tabs",
            "select_tab",
        ] {
            let call = call("firefox", json!({"action": action}));
            assert!(
                tool_label(&call, &result("{}"), None).starts_with(&format!("firefox {action} —")),
                "missing action in {action} label"
            );
        }
    }

    #[test]
    fn summarizes_firefox_navigation_from_returned_url_then_arguments() {
        let call = call(
            "firefox",
            json!({"action":"navigate","url":"https://example.com/fallback"}),
        );
        assert_eq!(
            tool_label(
                &call,
                &result(r#"{"url":"https://amazon.com/product/1"}"#),
                None
            ),
            "firefox navigate — opened amazon.com"
        );
        assert_eq!(
            tool_label(&call, &result("not json"), None),
            "firefox navigate — opened example.com"
        );
    }

    #[test]
    fn summarizes_firefox_actions_from_results_with_argument_fallbacks() {
        let click = call("firefox", json!({"action":"click","ref":"button-4"}));
        assert_eq!(
            tool_label(&click, &result(r#"{"label":"Add to Cart"}"#), None),
            "firefox click — clicked “Add to Cart”"
        );
        assert_eq!(
            tool_label(&click, &result(r#"{"clicked":"button-4"}"#), None),
            "firefox click — clicked ref button-4"
        );

        let typed = call("firefox", json!({"action":"type","ref":"search"}));
        assert_eq!(
            tool_label(
                &typed,
                &result(r#"{"typed":"search","value":"shoes"}"#),
                None
            ),
            "firefox type — typed into ref search"
        );

        let press = call("firefox", json!({"action":"press","key":"Enter"}));
        assert_eq!(
            tool_label(&press, &result(r#"{"pressed":"Enter"}"#), None),
            "firefox press — pressed Enter"
        );

        let scroll = call("firefox", json!({"action":"scroll"}));
        assert_eq!(
            tool_label(&scroll, &result(r#"{"x":0,"y":640}"#), None),
            "firefox scroll — scrolled to 0, 640"
        );
    }

    #[test]
    fn structured_firefox_failure_sets_error_state_and_label() {
        let call = call("firefox", json!({"action":"click","ref":"old-ref"}));
        let result = result(r#"{"ok":false,"error":"stale_ref","message":"element ref expired"}"#);
        let row = build_tool_row(&call, &result, None);

        assert_eq!(
            row.tool.as_ref().unwrap().label,
            "firefox click — failed: element ref expired"
        );
        assert!(row.tool.unwrap().is_error);
    }

    #[test]
    fn firefox_error_result_uses_plain_content_and_cleans_newlines() {
        let call = call("firefox", json!({"action":"navigate"}));
        let mut result = result("Firefox did not respond\nwithin 30 seconds");
        result.is_error = true;

        assert_eq!(
            tool_label(&call, &result, None),
            "firefox navigate — failed: Firefox did not respond within 30 seconds"
        );
    }

    #[test]
    fn firefox_empty_and_malformed_successes_use_factual_fallbacks() {
        let observe = call("firefox", json!({"action":"observe"}));
        assert_eq!(
            tool_label(&observe, &result(""), None),
            "firefox observe — observed page"
        );

        let click = call("firefox", json!({"action":"click"}));
        assert_eq!(
            tool_label(&click, &result("not json"), None),
            "firefox click — click completed"
        );
    }
}

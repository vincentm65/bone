//! Mirrors the daemon-owned `ToolDisplayConfig` (core-local, not in
//! `bone-protocol`) so the native transcript can render customized tool rows
//! without depending on `bone-core`.
//!
//! The daemon ships the resolved per-tool display config as opaque JSON in
//! `FrontendState.tool_display`. [`parse_map`] turns that payload into a
//! name→config map and [`custom_label`] mirrors the TUI's `tool_label` so a tool
//! with a config gets the same heading (template / `args` / `value_labels` /
//! shell formatting / read-file summary). A tool with no config returns `None`,
//! which the caller renders with its generic heading.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Mirror of `bone_core::tools::types::ToolDisplayConfig`. The daemon stays the
/// source of truth; this copy only lets the native UI read the opaque payload.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolDisplayConfig {
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub template: Option<String>,
    #[serde(default)]
    pub value_labels: HashMap<String, HashMap<String, String>>,
    #[serde(default)]
    pub show: Option<bool>,
    #[serde(default)]
    pub show_result: Option<bool>,
    #[serde(default)]
    pub eager: Option<bool>,
}

/// Parse the opaque `FrontendState.tool_display` payload into a name→config map.
/// A malformed payload degrades to an empty map (generic rendering).
pub fn parse_map(value: &Value) -> HashMap<String, ToolDisplayConfig> {
    serde_json::from_value(value.clone()).unwrap_or_default()
}

/// File and shell calls always get useful default headings. Unknown tools
/// without a config return `None` for the generic name fallback. `Some("")` means
/// the config hides the label (`show = false`).
pub fn custom_label(
    name: &str,
    arguments: &Value,
    content: &str,
    is_error: bool,
    display: Option<&ToolDisplayConfig>,
) -> Option<String> {
    let defaults = ToolDisplayConfig::default();
    let display = match display {
        Some(display) => display,
        None if matches!(
            name,
            "shell" | "read_file" | "create_file" | "write_file" | "edit_file"
        ) =>
        {
            &defaults
        }
        None => return None,
    };
    if display.show == Some(false) {
        return Some(String::new());
    }
    let mut label = tool_label(name, arguments, content, is_error, display);
    // File targets stay visible even when a custom template omits the path.
    if is_file_tool(name)
        && let Some(path) = file_target(name, arguments, content)
        && !label.contains(path)
    {
        label.push(' ');
        label.push_str(path);
    }
    if name == "edit_file"
        && !is_error
        && let Some((_, counts)) = content
            .trim_start()
            .lines()
            .next()
            .and_then(|line| line.rsplit_once(" (-"))
    {
        let summary = format!("(-{counts}");
        if !label.contains(&summary) {
            label.push(' ');
            label.push_str(&summary);
        }
    }
    Some(label)
}

fn is_file_tool(name: &str) -> bool {
    matches!(
        name,
        "read_file" | "create_file" | "write_file" | "edit_file"
    )
}

/// Results can retain a target even in a bounded history with no matching call.
fn file_target<'a>(name: &str, arguments: &'a Value, content: &'a str) -> Option<&'a str> {
    for key in ["path", "file_path", "filename"] {
        if let Some(path) = arguments
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            return Some(path);
        }
    }
    let first = content.trim_start().lines().next()?;
    match name {
        "read_file" => first.strip_prefix("File: "),
        "create_file" | "write_file" => first
            .strip_prefix("wrote ")?
            .rsplit_once(" (")
            .map(|(path, _)| path),
        "edit_file" => first
            .strip_prefix("edit_file ")?
            .rsplit_once(" (-")
            .map(|(path, _)| path),
        _ => None,
    }
}

/// Mirror of the TUI `tool_label`: shell calls format their command; a config
/// template/`args` list wins; otherwise `name` + `path`/`query` target, with a
/// line-count summary for `read_file`.
fn tool_label(
    name: &str,
    arguments: &Value,
    content: &str,
    is_error: bool,
    display: &ToolDisplayConfig,
) -> String {
    if name == "shell" {
        return format_shell_call_label(arguments);
    }

    if let Some(label) = format_display_label(name, arguments, display) {
        return label;
    }

    let target = if is_file_tool(name) {
        file_target(name, arguments, content)
    } else {
        arguments
            .get("path")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or_else(|| arguments.get("query").and_then(Value::as_str))
    };

    let mut label = match target {
        Some(target) => format!("{name} {target}"),
        None => name.to_string(),
    };

    if name == "read_file" && !is_error && !content.is_empty() {
        label.push_str(&read_file_line_summary(arguments, content));
    }

    label
}

/// Truncate to `max` chars on a char boundary, appending an ellipsis.
fn truncate_label(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let kept: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}…")
}

fn format_display_label(
    name: &str,
    arguments: &Value,
    display: &ToolDisplayConfig,
) -> Option<String> {
    if let Some(template) = display.template.as_deref()
        && let Some(rendered) = render_display_template(template, arguments, display)
        && !rendered.trim().is_empty()
    {
        return Some(format!("{name} {}", rendered.trim()));
    }

    let parts = display
        .args
        .iter()
        .filter_map(|arg| {
            arguments
                .get(arg)
                .filter(|value| !value.is_null())
                .map(|value| {
                    format!(
                        "{arg}={}",
                        format_labeled_display_value(arg, value, display)
                    )
                })
        })
        .collect::<Vec<_>>();

    if parts.is_empty() {
        None
    } else {
        Some(format!("{name} {}", parts.join(" ")))
    }
}

/// Render a display template. Scalar placeholders `{key}` interpolate argument
/// values; an array placeholder `{name[].f1|f2}` expands to the first present
/// field (`f1` then `f2`…) of each element of array arg `name`, each value
/// cleaned/truncated/quoted and joined with `, `.
///
/// Returns `None` when the template contains an array placeholder that resolves
/// to nothing (array arg absent or empty) — this lets a "list" template apply
/// only when the list is present, falling back to the `args` label instead.
fn render_display_template(
    template: &str,
    arguments: &Value,
    display: &ToolDisplayConfig,
) -> Option<String> {
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
        } else if let Some(value) = map.and_then(|map| map.get(key)) {
            out.push_str(&format_labeled_display_value(key, value, display));
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

fn format_labeled_display_value(arg: &str, value: &Value, display: &ToolDisplayConfig) -> String {
    value
        .as_str()
        .and_then(|value| display.value_labels.get(arg)?.get(value))
        .cloned()
        .unwrap_or_else(|| format_display_value(value))
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

/// A read_file content row: plain `   N | text` or hashline `N#HH|text`.
fn is_numbered_read_row(line: &str) -> bool {
    let line = line.trim_start();
    let rest = line.trim_start_matches(|c: char| c.is_ascii_digit());
    rest.len() < line.len() && (rest.starts_with(" | ") || rest.starts_with('#'))
}

fn read_file_line_summary(arguments: &Value, content: &str) -> String {
    // Current read_file output has metadata lines followed by numbered rows
    // (`N | text`, or `N#HH|text` in hashline mode). Count only those rows,
    // not the File/Range/Note headers.
    let numbered_lines = content
        .lines()
        .filter(|line| is_numbered_read_row(line))
        .count();
    if content.starts_with("File: ") {
        if numbered_lines == 0 {
            return " (0 lines)".to_string();
        }
        let start_line = arguments["start_line"].as_u64().unwrap_or(1) as usize;
        let end_line = start_line + numbered_lines - 1;
        return format!(" (lines {start_line}-{end_line}, {numbered_lines} read)");
    }

    // The result ends with a bracketed status footer ("\n\n[...]") that is
    // not file content; don't count it toward lines read.
    let content = content
        .rsplit_once("\n\n[")
        .filter(|(_, tail)| tail.ends_with(']') && !tail.contains('\n'))
        .map(|(body, _)| body)
        .unwrap_or(content);
    let lines_read =
        if content.starts_with('[') && content.ends_with(']') && !content.contains('\n') {
            0
        } else {
            content.lines().count()
        };
    if lines_read == 0 {
        return " (0 lines)".to_string();
    }

    let start_line = arguments["start_line"].as_u64().unwrap_or(1) as usize;
    let end_line = start_line + lines_read - 1;
    format!(" (lines {start_line}-{end_line}, {lines_read} read)")
}

fn format_shell_call_label(arguments: &Value) -> String {
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

fn format_shell_label(command: &str) -> String {
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

fn format_shell_command(command: &str) -> Vec<String> {
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
    use super::*;
    use serde_json::json;

    #[test]
    fn file_targets_are_visible_without_configs_and_with_targetless_templates() {
        for name in ["read_file", "create_file", "write_file", "edit_file"] {
            let args = json!({"path": "src/space name.rs"});
            let label = custom_label(name, &args, "", false, None).unwrap();
            assert_eq!(label, format!("{name} src/space name.rs"));
            let config = ToolDisplayConfig {
                template: Some("working".into()),
                ..Default::default()
            };
            assert!(
                custom_label(name, &args, "", false, Some(&config))
                    .unwrap()
                    .contains("src/space name.rs")
            );
        }
        let shell =
            custom_label("shell", &json!({"command": "cargo test"}), "", false, None).unwrap();
        assert_eq!(shell, "shell cargo test");
    }

    #[test]
    fn orphaned_file_results_retain_their_targets() {
        for (name, result) in [
            (
                "read_file",
                "File: /work/src/main.rs\nRange: 1-2\n1 | a\n2 | b",
            ),
            ("create_file", "wrote /work/src/main.rs (20 bytes, 2 lines)"),
            ("write_file", "wrote /work/src/main.rs (20 bytes, 2 lines)"),
            (
                "edit_file",
                "\n    edit_file /work/src/main.rs (-1 | +1)\n    1 - old\n    1 + new",
            ),
        ] {
            assert!(
                custom_label(name, &Value::Null, result, false, None)
                    .unwrap()
                    .contains("/work/src/main.rs")
            );
        }
    }

    #[test]
    fn display_config_round_trips_fixture() {
        let value = json!({
            "task_loop": {
                "args": ["action", "name"],
                "template": "{action} {name}",
                "value_labels": {"action": {"add": "adding"}},
                "show": false,
                "show_result": false,
                "eager": true
            }
        });
        let map = parse_map(&value);
        let config = map.get("task_loop").expect("config parsed");
        assert_eq!(config.args, vec!["action".to_string(), "name".to_string()]);
        assert_eq!(config.template.as_deref(), Some("{action} {name}"));
        assert_eq!(config.show, Some(false));
        assert_eq!(config.show_result, Some(false));
        assert_eq!(config.eager, Some(true));
        assert_eq!(
            config.value_labels.get("action").and_then(|m| m.get("add")),
            Some(&"adding".to_string())
        );
        // Round-trip back to JSON is lossless.
        let back = serde_json::to_value(config).unwrap();
        assert_eq!(back, value["task_loop"]);
    }

    #[test]
    fn malformed_payload_degrades_to_empty_map() {
        assert!(parse_map(&json!("not an object")).is_empty());
        assert!(parse_map(&json!({})).is_empty());
    }

    #[test]
    fn template_and_value_labels_build_custom_label() {
        let config = ToolDisplayConfig {
            template: Some("{action} {name}".into()),
            value_labels: HashMap::from([(
                "action".to_string(),
                HashMap::from([("add".to_string(), "adding".to_string())]),
            )]),
            ..Default::default()
        };
        let label = custom_label(
            "task_loop",
            &json!({"action": "add", "name": "deploy"}),
            "",
            false,
            Some(&config),
        );
        assert_eq!(label.as_deref(), Some("task_loop adding deploy"));
    }

    #[test]
    fn args_label_joins_present_arguments() {
        let config = ToolDisplayConfig {
            args: vec!["path".into(), "query".into()],
            ..Default::default()
        };
        let label = custom_label(
            "search",
            &json!({"path": "src", "other": 1}),
            "",
            false,
            Some(&config),
        );
        assert_eq!(label.as_deref(), Some("search path=src"));
    }

    #[test]
    fn empty_array_template_falls_back_to_args() {
        let config = ToolDisplayConfig {
            template: Some("{agents[].name|role}".into()),
            args: vec!["action".into()],
            ..Default::default()
        };
        // Array placeholder resolves empty → template ignored → args label.
        let label = custom_label(
            "dispatch",
            &json!({"action": "list", "agents": []}),
            "",
            false,
            Some(&config),
        );
        assert_eq!(label.as_deref(), Some("dispatch action=list"));

        // Present array placeholder expands, picking `name` then falling back to
        // `role` per element (the second element has no `name`).
        let label = custom_label(
            "dispatch",
            &json!({"action": "spawn", "agents": [{"name": "a\nb"}, {"role": "r"}]}),
            "",
            false,
            Some(&config),
        );
        assert_eq!(label.as_deref(), Some("dispatch \"a b\", \"r\""));
    }

    #[test]
    fn no_config_returns_none_and_show_false_hides_label() {
        assert!(custom_label("custom_tool", &json!({}), "", false, None).is_none());

        let hidden = ToolDisplayConfig {
            show: Some(false),
            ..Default::default()
        };
        assert_eq!(
            custom_label("task_loop", &json!({}), "", false, Some(&hidden)).as_deref(),
            Some("")
        );
    }

    #[test]
    fn read_file_summary_counts_numbered_lines() {
        let content = "File: a.rs\nRange: 1-2\n1 | a\n2 | b";
        let label = custom_label(
            "read_file",
            &json!({"path": "a.rs", "start_line": 1}),
            content,
            false,
            Some(&ToolDisplayConfig::default()),
        );
        assert_eq!(label.as_deref(), Some("read_file a.rs (lines 1-2, 2 read)"));

        let hashline = "File: a.rs\nRange: lines 5-7 of 9.\n5#k3|a\n6#--|long  [not editable]\n7#t6|";
        let label = custom_label(
            "read_file",
            &json!({"path": "a.rs", "start_line": 5}),
            hashline,
            false,
            Some(&ToolDisplayConfig::default()),
        );
        assert_eq!(label.as_deref(), Some("read_file a.rs (lines 5-7, 3 read)"));
    }

    #[test]
    fn shell_label_formats_command_and_expands_heredoc() {
        let label = custom_label(
            "shell",
            &json!({"action": "run", "command": "echo hi"}),
            "",
            false,
            Some(&ToolDisplayConfig::default()),
        );
        assert_eq!(label.as_deref(), Some("shell echo hi"));

        let heredoc = format_shell_label("cat <<EOF {a;b}\nEOF");
        assert_eq!(heredoc, "shell cat <<EOF\n  {\n    a;\n    b\n  }\n EOF");
    }
}

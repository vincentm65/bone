// ---------------------------------------------------------------------------
// Tool display helpers — build Message structs from tool calls/results.
// ---------------------------------------------------------------------------

use crate::chat::Message;
use crate::tools::types::{ToolCall, ToolResult};

pub fn build_tool_row(call: &ToolCall, result: &ToolResult) -> Message {
    Message::tool_row(tool_label(call, result), result.is_error)
}

pub fn tool_label(call: &ToolCall, result: &ToolResult) -> String {
    let target = match call.name.as_str() {
        "read_file" | "write_file" | "edit_file" => call.arguments["path"].as_str(),
        "bash" => call.arguments["command"].as_str(),
        _ => None,
    };

    let mut label = match target {
        Some(target) if !target.is_empty() => format!("{} {}", call.name, target),
        _ => call.name.clone(),
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

//! Converting a loaded conversation into rendered rows, shared so a restored
//! conversation looks identical in every frontend and identical to one built
//! turn by turn.

use bone_protocol::{ChatMessage, ChatRole, ToolCall, ToolDisplayConfig, ToolResult};

use crate::Message;
use crate::tool_display::build_tool_row;

/// Convert a loaded transcript into rows: tool rows are relabelled from the
/// originating call's arguments via [`build_tool_row`], and `edit_file`
/// renders its diff preview (embedded in the persisted result content).
/// `display` supplies each tool's registered display config.
pub fn transcript_rows<'a>(
    transcript: &[ChatMessage],
    display: impl Fn(&ToolCall) -> Option<&'a ToolDisplayConfig>,
) -> Vec<Message> {
    // Map each tool_call_id to its originating call so a tool-result row can
    // be relabelled from the call's `arguments`, matching the live path.
    let calls: std::collections::HashMap<&str, &ToolCall> = transcript
        .iter()
        .flat_map(|m| m.tool_calls.iter())
        .map(|c| (c.id.as_str(), c))
        .collect();
    let mut rows = Vec::new();
    for msg in transcript {
        match msg.role {
            ChatRole::User => {
                if msg.is_synthetic_relay() {
                    // A runtime relay of tool-returned images: the user did
                    // not type it, so show it as ambient text, not a prompt.
                    rows.push(Message::system(msg.content.clone()));
                } else {
                    rows.push(Message::user_with_images(
                        msg.content.clone(),
                        msg.images.len(),
                    ));
                }
            }
            ChatRole::Assistant => {
                if let Some(message) = assistant_display_message(&msg.content) {
                    rows.push(message);
                }
            }
            ChatRole::Tool => {
                if let Some(diff) = edit_diff_message(
                    msg.name.as_deref().unwrap_or_default(),
                    msg.is_error,
                    &msg.content,
                ) {
                    rows.push(diff);
                    continue;
                }
                let row = match msg.tool_call_id.as_deref().and_then(|id| calls.get(id)) {
                    Some(call) => build_tool_row(
                        call,
                        &ToolResult {
                            content: msg.content.clone(),
                            images: msg.images.clone(),
                            is_error: msg.is_error,
                            ..Default::default()
                        },
                        display(call),
                    ),
                    None => {
                        let label = msg.name.clone().unwrap_or_else(|| "tool".to_string());
                        let Some(mut row) = orphaned_tool_result_row(label, msg.is_error) else {
                            continue;
                        };
                        row.image_count = msg.images.len();
                        row
                    }
                };
                rows.push(row);
            }
            ChatRole::System => {}
        }
    }
    rows
}

pub fn assistant_display_message(content: &str) -> Option<Message> {
    let content = crate::timing::strip_timing_blocks(content);
    (!content.trim().is_empty()).then(|| Message::assistant(content))
}

pub fn edit_diff_message(name: &str, is_error: bool, content: &str) -> Option<Message> {
    if name != "edit_file" || is_error || !content.starts_with("Edited: ") {
        return None;
    }
    let newline = content.find('\n')?;
    Some(Message::system(content[newline..].to_string()))
}

/// An unmatched successful result has no useful label or content to render.
/// Keep unmatched errors visible, but do not leak bare tool names into the UI.
pub fn orphaned_tool_result_row(name: String, is_error: bool) -> Option<Message> {
    is_error.then(|| Message::tool_row(name, true))
}

/// Render a point-in-time view of a running job from its bounded runtime-event log.
pub fn job_messages<'a>(
    job: &bone_protocol::JobSnapshot,
    display: impl Fn(&ToolCall) -> Option<&'a ToolDisplayConfig>,
) -> Vec<Message> {
    let mut rows = vec![Message::user(job.task.clone())];
    let mut answer = String::new();
    let mut timing_filter = crate::timing::TimingBlockFilter::default();
    let mut calls = std::collections::HashMap::new();
    let mut shown_edit_previews = std::collections::HashSet::new();
    for event in &job.events {
        match event {
            bone_protocol::JobEventSnapshot::TextDelta { text } => {
                answer.push_str(&timing_filter.push(text));
            }
            bone_protocol::JobEventSnapshot::ReasoningDelta { text } if !text.is_empty() => {
                rows.push(Message::system(format!("thinking: {text}")));
            }
            bone_protocol::JobEventSnapshot::ToolCall {
                id,
                name,
                arguments,
                edit_preview,
            } => {
                answer.push_str(&timing_filter.finish());
                if let Some(message) = assistant_display_message(&answer) {
                    rows.push(message);
                }
                answer.clear();
                calls.insert(
                    id.clone(),
                    ToolCall {
                        id: id.clone(),
                        name: name.clone(),
                        arguments: arguments.clone(),
                    },
                );
                if let Some(diff) = edit_preview {
                    rows.push(Message::system(diff.clone()));
                    shown_edit_previews.insert(id.clone());
                }
            }
            bone_protocol::JobEventSnapshot::ToolResult {
                name,
                call_id,
                content,
                is_error,
            } => {
                if shown_edit_previews.contains(call_id) && !is_error {
                    continue;
                }
                if let Some(diff) = edit_diff_message(name, *is_error, content) {
                    rows.push(diff);
                    continue;
                }
                let result = ToolResult {
                    call_id: call_id.clone(),
                    name: name.clone(),
                    content: content.clone(),
                    is_error: *is_error,
                    ..Default::default()
                };
                let row = match calls.get(call_id) {
                    Some(call) => build_tool_row(call, &result, display(call)),
                    None => {
                        let Some(row) = orphaned_tool_result_row(name.clone(), *is_error) else {
                            continue;
                        };
                        row
                    }
                };
                rows.push(row);
            }
            bone_protocol::JobEventSnapshot::Failed { message } => {
                rows.push(Message::system(format!("failed: {message}")))
            }
            bone_protocol::JobEventSnapshot::ReasoningDelta { .. } => {}
        }
    }
    answer.push_str(&timing_filter.finish());
    if let Some(message) = assistant_display_message(&answer) {
        rows.push(message);
    }
    if rows.len() == 1 {
        let status = job.activity.as_deref().unwrap_or("starting");
        rows.push(Message::system(format!("{} — {status}", job.id)));
    }
    rows
}

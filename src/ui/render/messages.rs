use std::io;

use crate::chat::{Message, ToolDisplay};
use crate::llm::ChatRole;
use crate::ui::render::BoneTerminal;
use crate::ui::theme::Theme;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

/// Convert raw strings into terminal lines for native scrollback rendering.
pub fn insert_raw_lines(term: &mut BoneTerminal, lines: &[&str]) -> io::Result<()> {
    if lines.is_empty() {
        return Ok(());
    }

    term.insert_before(lines.len() as u16, |buf| {
        for (row, line) in lines.iter().enumerate() {
            let area = ratatui::layout::Rect {
                x: 0,
                y: row as u16,
                width: buf.area.width,
                height: 1,
            };
            Paragraph::new((*line).to_string()).render(area, buf);
        }
    })
}

fn render_tool(tool: &ToolDisplay, theme: &Theme, lines: &mut Vec<Line<'static>>) {
    let marker = if tool.is_error { "✕ " } else { "  " };
    let marker_style = Style::default().fg(theme.tool_error);

    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(marker, marker_style),
        Span::styled(tool.label.clone(), Style::default().fg(theme.tool_call)),
    ]));
}

fn render_content(msg: &Message, theme: &Theme, lines: &mut Vec<Line<'static>>) {
    match msg.role {
        ChatRole::User => {
            for (idx, raw_line) in msg.content.lines().enumerate() {
                let prefix = if idx == 0 { "> " } else { "  " };
                lines.push(Line::from(vec![
                    Span::styled(prefix, Style::default().fg(theme.user_msg)),
                    Span::styled(raw_line.to_string(), Style::default().fg(theme.user_msg)),
                ]));
            }
        }
        ChatRole::Assistant => {
            for raw_line in msg.content.lines() {
                lines.push(Line::raw(raw_line.to_string()));
            }
        }
        ChatRole::System | ChatRole::Tool => {
            for raw_line in msg.content.lines() {
                lines.push(Line::from(Span::styled(
                    raw_line.to_string(),
                    Style::default().fg(theme.system_msg),
                )));
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

/// Convert messages into terminal lines.
pub fn msg_to_lines(
    msgs: &[Message],
    theme: &Theme,
    prev_role: Option<ChatRole>,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut prev_role = prev_role;

    for msg in msgs {
        if role_changed(prev_role, msg.role) {
            lines.push(Line::raw(""));
        }

        if let Some(tool) = &msg.tool {
            render_tool(tool, theme, &mut lines);
        } else {
            render_content(msg, theme, &mut lines);
        }

        prev_role = Some(msg.role);
        lines.push(Line::raw(""));
    }

    if lines.is_empty() {
        lines.push(Line::raw(""));
    }

    lines
}

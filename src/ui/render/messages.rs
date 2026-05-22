use std::io;

use unicode_width::UnicodeWidthStr;

use crate::chat::{Message, ToolDisplay};
use crate::llm::ChatRole;
use crate::ui::render::{BoneTerminal, wrap};
use crate::ui::theme::Theme;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

/// Insert already-rendered visual lines into native scrollback.
pub fn insert_lines(term: &mut BoneTerminal, lines: &[Line<'static>]) -> io::Result<()> {
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
            Paragraph::new(line.clone()).render(area, buf);
        }
    })
}

pub fn assistant_raw_lines_to_lines(lines: &[&str], width: u16) -> Vec<Line<'static>> {
    lines
        .iter()
        .flat_map(|raw_line| wrap::wrap_text(raw_line, width as usize))
        .map(Line::raw)
        .collect()
}

fn render_tool(tool: &ToolDisplay, theme: &Theme, lines: &mut Vec<Line<'static>>, width: usize) {
    let marker = if tool.is_error { "✕ " } else { "  " };
    let label_style = Style::default().fg(theme.tool_call);
    let marker_style = Style::default().fg(theme.tool_error);
    let indent = "    ";
    let prefix_width = 4; // "  " + marker (2 chars) = 4 display cols
    let label_width = width.saturating_sub(prefix_width).max(1);

    let wrapped = wrap::wrap_text(&tool.label, label_width);

    for (i, visual_line) in wrapped.into_iter().enumerate() {
        if i == 0 {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(marker, marker_style),
                Span::styled(visual_line, label_style),
            ]));
        } else {
            lines.push(Line::from(vec![
                Span::raw(indent),
                Span::styled(visual_line.trim_start().to_string(), label_style),
            ]));
        }
    }
}

fn render_diff_preview(
    content: &str,
    theme: &Theme,
    lines: &mut Vec<Line<'static>>,
    terminal_width: usize,
) {
    let content = content.strip_prefix('\n').unwrap_or(content);
    for (idx, raw_line) in content.lines().enumerate() {
        let (line, style) = if idx == 0 {
            (raw_line.to_string(), Style::default().fg(theme.system_msg))
        } else if raw_line.len() >= 7 {
            match raw_line.as_bytes()[6] as char {
                '-' => (
                    pad_to_terminal_width(raw_line, terminal_width),
                    Style::default().bg(theme.diff_removed),
                ),
                '+' => (
                    pad_to_terminal_width(raw_line, terminal_width),
                    Style::default().bg(theme.diff_added),
                ),
                _ => (raw_line.to_string(), Style::default().fg(theme.system_msg)),
            }
        } else {
            (raw_line.to_string(), Style::default().fg(theme.system_msg))
        };
        lines.push(Line::from(Span::styled(line, style)));
    }
}

fn pad_to_terminal_width(line: &str, terminal_width: usize) -> String {
    let terminal_width = terminal_width.max(1);
    let width = UnicodeWidthStr::width(line);
    let padded_width = width.div_ceil(terminal_width) * terminal_width;
    // Pad with spaces to fill terminal width for full-width background coloring
    let pad = padded_width.saturating_sub(width);
    format!("{line}{}", " ".repeat(pad))
}

fn render_content(msg: &Message, theme: &Theme, lines: &mut Vec<Line<'static>>, width: u16) {
    if matches!(msg.role, ChatRole::System) && msg.content.starts_with("\n") {
        render_diff_preview(&msg.content, theme, lines, width as usize);
        return;
    }

    match msg.role {
        ChatRole::User => {
            for (idx, raw_line) in msg.content.lines().enumerate() {
                let first_prefix = if idx == 0 { "> " } else { "  " };
                for visual_line in
                    wrap::wrap_text_with_prefix(raw_line, first_prefix, "  ", width as usize)
                {
                    lines.push(Line::from(Span::styled(
                        visual_line,
                        Style::default().fg(theme.user_msg),
                    )));
                }
            }
        }
        ChatRole::Assistant => {
            for raw_line in msg.content.lines() {
                for visual_line in wrap::wrap_text(raw_line, width as usize) {
                    lines.push(Line::raw(visual_line));
                }
            }
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

/// Convert messages into terminal lines.
pub fn msg_to_lines(
    msgs: &[Message],
    theme: &Theme,
    prev_role: Option<ChatRole>,
    width: u16,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut prev_role = prev_role;

    for msg in msgs {
        if role_changed(prev_role, msg.role) {
            lines.push(Line::raw(""));
        }

        if let Some(tool) = &msg.tool {
            render_tool(tool, theme, &mut lines, width as usize);
        } else {
            render_content(msg, theme, &mut lines, width);
        }

        prev_role = Some(msg.role);
        lines.push(Line::raw(""));
    }

    if lines.is_empty() {
        lines.push(Line::raw(""));
    }

    lines
}

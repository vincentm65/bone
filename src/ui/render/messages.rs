use std::io;

use unicode_width::UnicodeWidthStr;

use crate::chat::{Message, ToolDisplay};
use crate::llm::ChatRole;
use crate::ui::render::{BoneTerminal, markdown, wrap};
use crate::ui::theme::Theme;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget, Wrap};

pub fn insert_lines(term: &mut BoneTerminal, lines: &[Line<'static>]) -> io::Result<()> {
    if lines.is_empty() {
        return Ok(());
    }

    let width = term.size()?.width.max(1);
    let row_count: u16 = lines
        .iter()
        .map(|line| wrapped_line_count(line, width))
        .sum();

    term.insert_before(row_count, |buf| {
        let mut row = 0u16;
        for line in lines {
            let height = wrapped_line_count(line, buf.area.width.max(1));
            let area = ratatui::layout::Rect {
                x: 0,
                y: row,
                width: buf.area.width,
                height,
            };
            Paragraph::new(line.clone())
                .wrap(Wrap { trim: false })
                .render(area, buf);
            row = row.saturating_add(height);
        }
    })
}

fn wrapped_line_count(line: &Line<'static>, width: u16) -> u16 {
    Paragraph::new(line.clone())
        .wrap(Wrap { trim: false })
        .line_count(width)
        .max(1) as u16
}

pub fn assistant_markdown_to_lines(content: &str, width: u16) -> Vec<Line<'static>> {
    markdown::render_markdown(content, width)
}

pub fn render_tool(
    tool: &ToolDisplay,
    content: &str,
    theme: &Theme,
    lines: &mut Vec<Line<'static>>,
    width: usize,
) {
    let marker = if tool.is_error { "✕ " } else { "  " };
    let name_style = Style::default().fg(Color::White);
    let rest_style = Style::default().fg(theme.tool_call);
    let marker_style = Style::default().fg(theme.tool_error);
    let indent = "    ";
    let prefix_width = 4;
    let label_width = width.saturating_sub(prefix_width).max(1);

    let wrapped = wrap_tool_label(&tool.label, label_width);

    for (i, visual_line) in wrapped.into_iter().enumerate() {
        if i == 0 {
            let p = visual_line.find(' ').unwrap_or(visual_line.len());
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(marker, marker_style),
                Span::styled(visual_line[..p].to_string(), name_style),
                Span::styled(visual_line[p..].to_string(), rest_style),
            ]));
        } else {
            lines.push(Line::from(vec![
                Span::raw(indent),
                Span::styled(visual_line, rest_style),
            ]));
        }
    }

    if !content.is_empty() {
        lines.push(Line::raw(""));
        for raw_line in content.lines() {
            for visual_line in wrap::wrap_text(raw_line, width) {
                lines.push(Line::from(Span::styled(
                    visual_line,
                    Style::default().fg(theme.system_msg),
                )));
            }
        }
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
                    let styled_line = pad_to_terminal_width(&visual_line, width as usize);
                    lines.push(Line::from(Span::styled(
                        styled_line,
                        Style::default().fg(theme.user_msg).bg(theme.user_msg_bg),
                    )));
                }
            }
        }
        ChatRole::Assistant => {
            let rendered = markdown::render_markdown(&msg.content, width);
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
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut prev_role = prev_role;

    for msg in msgs {
        // Skip invisible placeholders (e.g., empty assistant messages between
        // tool rounds) so they don't inject extra blank-line gaps.
        if msg.tool.is_none() && msg.content.is_empty() {
            prev_role = Some(msg.role);
            continue;
        }

        if role_changed(prev_role, msg.role) {
            lines.push(Line::raw(""));
        }

        if let Some(tool) = &msg.tool {
            render_tool(tool, &msg.content, theme, &mut lines, width as usize);
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

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

    if !tool.label.is_empty() {
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
    }

    if !content.is_empty() {
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
    for raw_line in content.lines() {
        let (visual_lines, style, fill_background) =
            if let Some((gutter, body, marker)) = numbered_diff_parts(raw_line) {
                let style = match marker {
                    '-' => Style::default().bg(theme.diff_removed),
                    '+' => Style::default().bg(theme.diff_added),
                    _ => Style::default().fg(theme.system_msg),
                };
                (
                    wrap_numbered_diff_line(gutter, body, terminal_width),
                    style,
                    matches!(marker, '-' | '+'),
                )
            } else {
                (
                    wrap::wrap_text(raw_line, terminal_width),
                    Style::default().fg(theme.system_msg),
                    false,
                )
            };

        for visual_line in visual_lines {
            let line = if fill_background {
                pad_to_terminal_width(&visual_line, terminal_width)
            } else {
                visual_line
            };
            lines.push(Line::from(Span::styled(line, style)));
        }
    }
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

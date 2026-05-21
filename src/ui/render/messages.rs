use std::io;

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use crate::chat::Message;
use crate::llm::ChatRole;
use crate::ui::render::BoneTerminal;
use crate::ui::theme::Theme;

/// Convert a Message into terminal lines for native scrollback rendering.
///
/// `prev_role` is the role of the message that precedes this one (if any).
/// An extra blank line is inserted when the role changes between user and
/// non-user (assistant/system), giving consistent visual spacing.
pub fn insert_raw_lines(term: &mut BoneTerminal, lines: &[&str]) -> io::Result<()> {
    if lines.is_empty() {
        return Ok(());
    }

    term.insert_before(lines.len() as u16, |buf| {
        for (row, line) in lines.iter().enumerate() {
            let area = ratatui::layout::Rect { x: 0, y: row as u16, width: buf.area.width, height: 1 };
            Paragraph::new((*line).to_string()).render(area, buf);
        }
    })
}

pub fn msg_to_lines(msg: &Message, theme: &Theme, prev_role: Option<ChatRole>) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    // Add an extra blank line when transitioning between user ↔ assistant/system.
    if let Some(prev) = prev_role {
        let changed = matches!(
            (prev, msg.role),
            (ChatRole::User, ChatRole::Assistant) |
            (ChatRole::User, ChatRole::System) |
            (ChatRole::User, ChatRole::Tool) |
            (ChatRole::Assistant, ChatRole::User) |
            (ChatRole::System, ChatRole::User) |
            (ChatRole::Tool, ChatRole::User)
        );
        if changed {
            lines.push(Line::raw(""));
        }
    }

    match msg.role {
        ChatRole::User => {
            for (idx, raw_line) in msg.content.lines().enumerate() {
                if idx == 0 {
                    lines.push(Line::from(vec![
                        Span::styled("> ", Style::default().fg(theme.user_msg)),
                        Span::styled(raw_line.to_string(), Style::default().fg(theme.user_msg)),
                    ]));
                } else {
                    lines.push(Line::from(vec![
                        Span::styled("  ", Style::default().fg(theme.user_msg)),
                        Span::styled(raw_line.to_string(), Style::default().fg(theme.user_msg)),
                    ]));
                }
            }
        }
        ChatRole::Assistant => {
            for raw_line in msg.content.lines() {
                lines.push(Line::raw(raw_line.to_string()));
            }
        }
        ChatRole::System | ChatRole::Tool => {
            for raw_line in msg.content.lines() {
                lines.push(Line::from(Span::styled(raw_line.to_string(), Style::default().fg(theme.system_msg))));
            }
        }
    }

    if lines.is_empty() {
        lines.push(Line::raw(""));
    }

    lines.push(Line::raw(""));
    lines
}


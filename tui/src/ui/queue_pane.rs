//! Renderer for queued user input.

use std::collections::VecDeque;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::pane_page::PanePage;

pub const PANE_SOURCE: &str = "queue";

pub fn render(queue: &VecDeque<String>, selected: usize) -> Option<PanePage> {
    if queue.is_empty() {
        return None;
    }

    let selected = selected.min(queue.len() - 1);
    let mut lines = Vec::with_capacity(queue.len() + 2);
    for (index, text) in queue.iter().enumerate() {
        let is_selected = index == selected;
        let mut summary = text.replace(['\n', '\r'], " ");
        if summary.chars().count() > 72 {
            summary = format!("{}...", summary.chars().take(69).collect::<String>());
        }
        let mut line = Line::from(vec![
            Span::styled(
                if is_selected { " › " } else { "   " },
                Style::default().fg(if is_selected {
                    Color::White
                } else {
                    Color::DarkGray
                }),
            ),
            Span::styled(
                format!("{}. ", index + 1),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(summary, Style::default().fg(Color::Gray)),
        ]);
        if is_selected {
            line = line.style(Style::default().bg(Color::DarkGray));
        }
        lines.push(line);
    }
    lines.push(Line::from(Span::styled(
        " ↑/↓ select  ⇧↑/⇧↓ reorder  Enter next  F2 edit  Del remove  Ctrl+D clear",
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    )));
    lines.push(Line::from(Span::styled(
        " Input: Enter = queue · Ctrl/Alt+Enter = steer",
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    )));

    let visible_rows: usize = 8;
    let scroll = selected.saturating_sub(visible_rows.saturating_sub(3));
    Some(PanePage {
        source: PANE_SOURCE.to_string(),
        title: format!("Queue ({})", queue.len()),
        content: lines,
        visible_rows,
        scroll,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_selected_queue_item_and_help() {
        let queue = VecDeque::from(["first".to_string(), "second\nline".to_string()]);
        let page = render(&queue, 1).unwrap();

        assert_eq!(page.title, "Queue (2)");
        assert_eq!(page.content.len(), 4);
        assert!(page.content[1].to_string().contains("second line"));
        assert!(page.content[2].to_string().contains("reorder"));
        assert!(page.content[3].to_string().contains("Ctrl/Alt+Enter"));
    }

    #[test]
    fn empty_queue_has_no_pane() {
        assert!(render(&VecDeque::new(), 0).is_none());
    }
}

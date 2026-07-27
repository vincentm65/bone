//! Renderer for queued user input.

use std::collections::VecDeque;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::pane_page::PanePage;
use super::theme::Theme;

pub const PANE_SOURCE: &str = "queue";

pub fn render(queue: &VecDeque<String>, selected: usize, theme: &Theme) -> Option<PanePage> {
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
                    theme.palette.accent
                } else {
                    theme.palette.muted
                }),
            ),
            Span::styled(
                format!("{}. ", index + 1),
                Style::default().fg(theme.palette.muted),
            ),
            Span::styled(summary, Style::default().fg(theme.palette.fg)),
        ]);
        if is_selected {
            line = line.style(Style::default().bg(theme.palette.selection));
        }
        lines.push(line);
    }
    lines.push(Line::from(Span::styled(
        " ↑/↓ select  ⇧↑/⇧↓ reorder  Enter next  F2 edit  Del remove  Ctrl+D clear",
        Style::default()
            .fg(theme.palette.muted)
            .add_modifier(Modifier::DIM),
    )));
    lines.push(Line::from(Span::styled(
        " Input: Enter = queue · Ctrl/Alt+Enter = steer",
        Style::default()
            .fg(theme.palette.muted)
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
        let mut theme = Theme::default();
        theme.palette.accent = ratatui::style::Color::Rgb(1, 2, 3);
        theme.palette.selection = ratatui::style::Color::Rgb(4, 5, 6);
        theme.palette.fg = ratatui::style::Color::Rgb(7, 8, 9);
        theme.palette.muted = ratatui::style::Color::Rgb(10, 11, 12);
        let page = render(&queue, 1, &theme).unwrap();

        assert_eq!(page.title, "Queue (2)");
        assert_eq!(page.content.len(), 4);
        assert_eq!(page.content[0].spans[0].style.fg, Some(theme.palette.muted));
        assert_eq!(page.content[0].spans[1].style.fg, Some(theme.palette.muted));
        assert_eq!(page.content[0].spans[2].style.fg, Some(theme.palette.fg));
        assert_eq!(page.content[1].style.bg, Some(theme.palette.selection));
        assert_eq!(
            page.content[1].spans[0].style.fg,
            Some(theme.palette.accent)
        );
        assert_eq!(page.content[1].spans[1].style.fg, Some(theme.palette.muted));
        assert_eq!(page.content[1].spans[2].style.fg, Some(theme.palette.fg));
        assert!(page.content[1].to_string().contains("second line"));
        assert!(page.content[2].to_string().contains("reorder"));
        assert_eq!(page.content[2].spans[0].style.fg, Some(theme.palette.muted));
        assert!(page.content[3].to_string().contains("Ctrl/Alt+Enter"));
        assert_eq!(page.content[3].spans[0].style.fg, Some(theme.palette.muted));
    }

    #[test]
    fn empty_queue_has_no_pane() {
        assert!(render(&VecDeque::new(), 0, &Theme::default()).is_none());
    }
}

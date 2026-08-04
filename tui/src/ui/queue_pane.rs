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
#[path = "queue_pane_tests.rs"]
mod tests;

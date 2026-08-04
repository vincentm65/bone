//! Native live pane for host-managed background processes.
use super::pane_page::PanePage;
use bone_protocol::ProcessSnapshot;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::ui::theme::Theme;

pub const PANE_SOURCE: &str = "processes";

pub fn render(
    theme: &Theme,
    processes: &[ProcessSnapshot],
    selected_id: Option<&str>,
) -> Option<PanePage> {
    let active: Vec<_> = processes.iter().filter(|process| process.running).collect();
    if active.is_empty() {
        return None;
    }

    let rows = active
        .iter()
        .map(|process| {
            let selected = Some(process.id.as_str()) == selected_id;
            let command = process.command.replace(['\n', '\r'], " ");
            let label: String = command.chars().take(48).collect();
            let tail = process
                .stdout
                .lines()
                .last()
                .or_else(|| process.stderr.lines().last())
                .unwrap_or("starting")
                .replace(['\n', '\r'], " ");
            let tail: String = tail.chars().take(40).collect();
            let line = Line::from(vec![
                Span::styled(
                    "◑ ",
                    Style::default()
                        .fg(theme.palette.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(label, Style::default().fg(theme.palette.fg)),
                Span::styled(
                    format!(" — {tail}"),
                    Style::default().fg(theme.palette.muted),
                ),
            ]);
            (selected, line)
        })
        .collect();

    Some(super::selectable_pane::render(
        theme,
        PANE_SOURCE,
        format!("Processes ({})", active.len()),
        rows,
    ))
}

#[cfg(test)]
#[path = "processes_pane_tests.rs"]
mod tests;

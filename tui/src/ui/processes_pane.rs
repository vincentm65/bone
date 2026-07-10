//! Native live pane for host-managed background processes.
use super::pane_page::PanePage;
use crate::processes::ProcessSnapshot;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

pub const PANE_SOURCE: &str = "processes";
pub fn render(processes: &[ProcessSnapshot]) -> Option<PanePage> {
    let active: Vec<_> = processes.iter().filter(|p| p.running).collect();
    if active.is_empty() {
        return None;
    }
    let mut lines = vec![Line::from(Span::styled(
        format!(" ◑ Processes ({})", active.len()),
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ))];
    for p in &active {
        let command: String = p
            .command
            .replace(['\n', '\r'], " ")
            .chars()
            .take(72)
            .collect();
        lines.push(Line::from(vec![
            Span::styled("   ◑ ", Style::default().fg(Color::Yellow)),
            Span::styled(command, Style::default().fg(Color::Gray)),
            Span::styled(format!("  {}", p.id), Style::default().fg(Color::DarkGray)),
        ]));
        let tail = p.stdout.lines().last().or_else(|| p.stderr.lines().last());
        if let Some(tail) = tail {
            lines.push(Line::from(Span::styled(
                format!("     {tail}"),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }
    lines.push(Line::raw(""));
    Some(PanePage {
        source: PANE_SOURCE.into(),
        title: format!("Processes ({})", active.len()),
        content: lines,
        visible_rows: 8,
        scroll: 0,
    })
}

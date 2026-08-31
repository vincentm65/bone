//! Native live pane for host-managed background processes.
use super::pane_page::PanePage;
use bone_protocol::ProcessSnapshot;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::ui::theme::Theme;

pub const PANE_SOURCE: &str = "processes";

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

fn elapsed_ms(process: &ProcessSnapshot) -> u64 {
    if process.started_at == 0 {
        return 0;
    }
    process
        .finished_at
        .unwrap_or_else(now_millis)
        .saturating_sub(process.started_at)
}

pub fn render(
    theme: &Theme,
    processes: &[ProcessSnapshot],
    selected_id: Option<&str>,
) -> Option<PanePage> {
    let visible: Vec<_> = processes.iter().collect();
    if visible.is_empty() {
        return None;
    }

    let rows = visible
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
            let state = match process.state {
                bone_protocol::ProcessState::Running => "running",
                bone_protocol::ProcessState::Exited => "exited",
                bone_protocol::ProcessState::TimedOut => "timed out",
                bone_protocol::ProcessState::Cancelled => "cancelled",
            };
            let elapsed_ms = elapsed_ms(process);
            let elapsed = if elapsed_ms < 60_000 {
                format!("{}s", elapsed_ms / 1000)
            } else {
                format!("{}m{}s", elapsed_ms / 60_000, (elapsed_ms / 1000) % 60)
            };
            let line = Line::from(vec![
                Span::styled(
                    "◑ ",
                    Style::default()
                        .fg(theme.palette.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(label, Style::default().fg(theme.palette.fg)),
                Span::styled(
                    format!(" [{state} · {elapsed}]"),
                    Style::default().fg(theme.palette.muted),
                ),
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
        format!("Processes ({})", visible.len()),
        rows,
    ))
}

#[cfg(test)]
#[path = "processes_pane_tests.rs"]
mod tests;

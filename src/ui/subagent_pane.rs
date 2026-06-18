//! Rust-side renderer for the sub-agent live pane.
//!
//! Renders directly from the job registry snapshot — no Lua involved — so
//! the pane stays live even while a Lua tool blocks the VM (e.g. a long
//! `ctx.agent.wait`).

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::ext::jobs::{Job, JobStatus};

use super::pane_page::PanePage;

/// Pane source identifier (stable key for upsert/remove).
pub const PANE_SOURCE: &str = "subagents";

/// Render the sub-agent pane for the registered `agents` from a registry
/// `jobs` snapshot. Only shows agents with running jobs.
/// Returns `None` when no agents are registered or all are idle.
pub fn render(agents: &[String], jobs: &[Job]) -> Option<PanePage> {
    if agents.is_empty() {
        return None;
    }

    let now = current_unix_seconds();
    let mut lines = Vec::new();

    for agent in agents {
        let agent_jobs: Vec<&Job> = jobs.iter().filter(|j| j.agent == *agent).collect();
        let running: Vec<&Job> = agent_jobs
            .iter()
            .filter(|j| j.status == JobStatus::Running)
            .copied()
            .collect();

        if running.is_empty() {
            continue;
        }

        if running.len() > 1 {
            // Multi-job template header.
            lines.push(Line::from(Span::styled(
                format!(" ◑ {} ({} running)", agent, running.len()),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )));
            for job in &running {
                let elapsed = now.saturating_sub(job.started_at);
                let mut task = job.task.replace(['\n', '\r'], " ");
                if task.chars().count() > 36 {
                    task = format!("{}...", task.chars().take(33).collect::<String>());
                }
                lines.push(Line::from(vec![
                    Span::styled("   ◑ ", Style::default().fg(Color::DarkGray)),
                    Span::styled(task, Style::default().fg(Color::Gray)),
                    Span::styled(
                        format!(
                            " ({}s) {}/{} in/out",
                            elapsed, job.token_sent, job.token_received
                        ),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
            }
        } else {
            let job = running[0];
            let (icon, status) = job_status(job, now);
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {icon} "),
                    Style::default()
                        .fg(icon_fg(job))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(agent.clone(), Style::default().fg(name_fg(job))),
                Span::styled(" ", Style::default().fg(Color::DarkGray)),
                Span::styled(status, Style::default().fg(Color::DarkGray)),
            ]));
        }
    }

    if lines.is_empty() {
        return None;
    }

    lines.push(Line::raw(""));

    Some(PanePage {
        source: PANE_SOURCE.to_string(),
        title: format!("Agents ({})", agents.len()),
        content: lines,
        visible_rows: 8,
        scroll: 0,
    })
}

fn icon_fg(job: &Job) -> Color {
    match job.status {
        JobStatus::Running => Color::White,
        JobStatus::Done => Color::DarkGray,
        JobStatus::Error => Color::Red,
    }
}

fn name_fg(job: &Job) -> Color {
    if job.status == JobStatus::Running {
        Color::White
    } else {
        Color::DarkGray
    }
}

/// Build `(icon, status-text)` for a single job.
fn job_status(job: &Job, now: u64) -> (&'static str, String) {
    match job.status {
        JobStatus::Running => {
            let elapsed = now.saturating_sub(job.started_at);
            let mut task = job.task.replace(['\n', '\r'], " ");
            if task.chars().count() > 40 {
                task = format!("{}...", task.chars().take(37).collect::<String>());
            }
            (
                "◑",
                format!(
                    "running {} ({}s) {}/{} in/out",
                    task, elapsed, job.token_sent, job.token_received
                ),
            )
        }
        JobStatus::Done => {
            if job.token_sent > 0 || job.token_received > 0 {
                (
                    "○",
                    format!("idle ({}/{} in/out)", job.token_sent, job.token_received),
                )
            } else {
                ("○", "idle".to_string())
            }
        }
        JobStatus::Error => ("✗", "error".to_string()),
    }
}

fn current_unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "subagent_pane_tests.rs"]
mod subagent_pane_tests;

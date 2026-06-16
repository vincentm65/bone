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
/// `jobs` snapshot. Returns `None` when no agents are registered.
pub fn render(agents: &[String], jobs: &[Job]) -> Option<PanePage> {
    if agents.is_empty() {
        return None;
    }

    let now = current_unix_seconds();
    let mut lines = Vec::with_capacity(agents.len());

    for agent in agents {
        let latest = latest_job_for(agent, jobs);
        let (icon, status) = job_status(latest, now);
        let is_idle = icon == "○" && status.starts_with("idle");

        let (icon_fg, name_fg) = if is_idle {
            (Color::DarkGray, Color::DarkGray)
        } else {
            (Color::White, Color::White)
        };

        lines.push(Line::from(vec![
            Span::styled(
                format!(" {icon} "),
                Style::default().fg(icon_fg).add_modifier(Modifier::BOLD),
            ),
            Span::styled(agent.clone(), Style::default().fg(name_fg)),
            Span::styled(" ", Style::default().fg(Color::DarkGray)),
            Span::styled(status, Style::default().fg(Color::DarkGray)),
        ]));
    }

    lines.push(Line::raw(""));

    Some(PanePage {
        source: PANE_SOURCE.to_string(),
        title: format!("Agents ({})", agents.len()),
        content: lines,
        visible_rows: 8,
        scroll: 0,
        interaction: None,
    })
}

/// Find the most recent job for an agent (highest numeric job id).
fn latest_job_for<'a>(agent: &str, jobs: &'a [Job]) -> Option<&'a Job> {
    jobs.iter()
        .filter(|j| j.agent == agent)
        .max_by_key(|j| job_id_number(&j.id))
}

/// Extract the numeric suffix of a `job-N` id (0 when malformed).
fn job_id_number(id: &str) -> u64 {
    id.rsplit('-')
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

/// Build `(icon, status-text)` for the latest job of an agent.
fn job_status(job: Option<&Job>, now: u64) -> (&'static str, String) {
    let job = match job {
        Some(j) => j,
        None => return ("○", "idle".to_string()),
    };

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

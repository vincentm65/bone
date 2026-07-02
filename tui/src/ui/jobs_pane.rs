//! Rust-side renderer for the background-jobs live pane.
//!
//! Renders directly from the job registry snapshot — no Lua involved — so
//! the pane stays live even while a Lua tool blocks the VM (e.g. a long
//! `ctx.agent.wait`). Any tool that dispatches background jobs via
//! `ctx.agent.spawn` (sub-agents, shotgun, …) surfaces here; the pane has no
//! knowledge of which tool produced a job beyond the `agent` label it carries.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::ext::jobs::{Job, JobStatus, current_unix_seconds};

use super::pane_page::PanePage;

/// Pane source identifier (stable key for upsert/remove).
pub const PANE_SOURCE: &str = "jobs";

/// Render the jobs pane from the registry snapshot, grouping by `agent` label.
/// Only shows agents with at least one running job (a completed job stays
/// visible while a sibling in the same group is still running).
/// Returns `None` when no jobs are active.
pub fn render(jobs: &[Job]) -> Option<PanePage> {
    let agents = pane_agents(jobs);
    if agents.is_empty() {
        return None;
    }

    let now = current_unix_seconds();
    let mut lines = Vec::new();
    let mut active_agent_count = 0usize;

    for agent in &agents {
        let agent_jobs: Vec<&Job> = jobs.iter().filter(|j| j.agent == *agent).collect();
        let active: Vec<&Job> = agent_jobs
            .iter()
            .filter(|j| !j.is_finished())
            .copied()
            .collect();

        if active.is_empty() {
            continue;
        }
        active_agent_count += 1;

        let visible_jobs: Vec<&Job> = agent_jobs
            .iter()
            .filter(|j| !j.is_finished() || !j.consumed)
            .copied()
            .collect();

        if visible_jobs.len() > 1 {
            // Multi-job template header.
            lines.push(Line::from(Span::styled(
                format!(
                    " ◑ {} ({} active, {} done)",
                    agent,
                    active.len(),
                    visible_jobs.iter().filter(|j| j.is_finished()).count()
                ),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )));
            for job in &visible_jobs {
                let mut task = job_label(job).replace(['\n', '\r'], " ");
                if task.chars().count() > 36 {
                    task = format!("{}...", task.chars().take(33).collect::<String>());
                }
                if let Some(activity) = &job.activity {
                    task = activity.replace(['\n', '\r'], " ");
                    if task.chars().count() > 36 {
                        task = format!("{}...", task.chars().take(33).collect::<String>());
                    }
                }
                let total = job.token_sent + job.token_received;
                let mut parts = vec![
                    Span::styled(
                        format!("   {} ", job_status_icon(job)),
                        Style::default().fg(icon_fg(job)),
                    ),
                    Span::styled(task, Style::default().fg(Color::Gray)),
                ];
                let elapsed = match job.status {
                    JobStatus::Running => Some(now.saturating_sub(job.started_at)),
                    _ => None,
                };
                if let Some(elapsed) = elapsed {
                    parts.push(Span::styled(
                        format!(" ({}s) {} total", elapsed, format_tokens(total)),
                        Style::default().fg(Color::DarkGray),
                    ));
                } else {
                    parts.push(Span::styled(
                        format!(" {} total", format_tokens(total)),
                        Style::default().fg(icon_fg(job)),
                    ));
                }
                lines.push(Line::from(parts));
            }
        } else {
            let job = active[0];
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
        title: format!("Agents ({active_agent_count})"),
        content: lines,
        visible_rows: 8,
        scroll: 0,
    })
}

/// Unique, first-seen-ordered `agent` labels present in the job snapshot.
fn pane_agents(jobs: &[Job]) -> Vec<String> {
    let mut names = Vec::new();
    for job in jobs {
        if !job.agent.is_empty() && !names.iter().any(|name| name == &job.agent) {
            names.push(job.agent.clone());
        }
    }
    names
}

/// Display label for a job: the model-supplied title when present, otherwise
/// the raw task prompt (truncated by callers).
fn job_label(job: &Job) -> &str {
    if job.title.is_empty() {
        &job.task
    } else {
        &job.title
    }
}

fn icon_fg(job: &Job) -> Color {
    match job.status {
        JobStatus::Running => Color::White,
        JobStatus::Queued => Color::Yellow,
        JobStatus::Done => Color::DarkGray,
        JobStatus::Error => Color::Red,
    }
}

fn name_fg(job: &Job) -> Color {
    if !job.is_finished() {
        Color::White
    } else {
        Color::DarkGray
    }
}

fn job_status_icon(job: &Job) -> &'static str {
    match job.status {
        JobStatus::Running => "◑",
        JobStatus::Queued => "⧗",
        JobStatus::Done => "✓",
        JobStatus::Error => "✗",
    }
}

/// Build `(icon, status-text)` for a single job.
fn job_status(job: &Job, now: u64) -> (&'static str, String) {
    match job.status {
        JobStatus::Queued => ("⧗", "queued".to_string()),
        JobStatus::Running => {
            let elapsed = now.saturating_sub(job.started_at);
            let mut task = job
                .activity
                .as_deref()
                .unwrap_or_else(|| job_label(job))
                .replace(['\n', '\r'], " ");
            if task.chars().count() > 40 {
                task = format!("{}...", task.chars().take(37).collect::<String>());
            }
            (
                job_status_icon(job),
                format!(
                    "{} ({}s) {} total",
                    task,
                    elapsed,
                    format_tokens(job.token_sent + job.token_received)
                ),
            )
        }
        JobStatus::Done => {
            if job.token_sent > 0 || job.token_received > 0 {
                (
                    "○",
                    format!(
                        "idle ({} total)",
                        format_tokens(job.token_sent + job.token_received)
                    ),
                )
            } else {
                ("○", "idle".to_string())
            }
        }
        JobStatus::Error => ("✗", "error".to_string()),
    }
}

fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.2}m", n as f64 / 1_000_000.0)
    } else if n >= 10_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else if n >= 1_000 {
        let s = n.to_string();
        let mut out = String::with_capacity(s.len() + s.len() / 3);
        for (i, c) in s.chars().rev().enumerate() {
            if i > 0 && i % 3 == 0 {
                out.push(',');
            }
            out.push(c);
        }
        out.chars().rev().collect()
    } else {
        n.to_string()
    }
}

#[cfg(test)]
#[path = "jobs_pane_tests.rs"]
mod jobs_pane_tests;

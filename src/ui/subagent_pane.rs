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
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {icon} "),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(agent.clone(), Style::default().fg(Color::White)),
            Span::styled(" ", Style::default().fg(Color::DarkGray)),
            Span::styled(status, Style::default().fg(Color::DarkGray)),
        ]));
    }

    Some(PanePage {
        source: PANE_SOURCE.to_string(),
        title: format!("Agents ({})", agents.len()),
        content: lines,
        visible_rows: 8,
        scroll: 0,
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
    id.rsplit('-').next().and_then(|n| n.parse().ok()).unwrap_or(0)
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
mod tests {
    use super::*;

    fn job(id: &str, agent: &str, status: JobStatus) -> Job {
        Job {
            id: id.to_string(),
            agent: agent.to_string(),
            task: "do something".to_string(),
            status,
            result: None,
            started_at: current_unix_seconds(),
            finished_at: None,
            consumed: false,
            token_sent: 0,
            token_received: 0,
            result_file: None,
        }
    }

    #[test]
    fn render_returns_none_without_agents() {
        assert!(render(&[], &[]).is_none());
    }

    #[test]
    fn render_lists_all_agents() {
        let agents = vec!["researcher".to_string(), "coder".to_string()];
        let pane = render(&agents, &[]).unwrap();
        assert_eq!(pane.source, PANE_SOURCE);
        assert_eq!(pane.title, "Agents (2)");
        assert_eq!(pane.content.len(), 2);
        // Idle agents show the idle marker.
        let first: String = pane.content[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(first.contains("○"), "expected idle icon: {first}");
        assert!(first.contains("researcher"));
        assert!(first.contains("idle"));
    }

    #[test]
    fn render_shows_running_status() {
        let agents = vec!["researcher".to_string()];
        let jobs = vec![job("job-1", "researcher", JobStatus::Running)];
        let pane = render(&agents, &jobs).unwrap();
        let line: String = pane.content[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(line.contains("◑"), "expected running icon: {line}");
        assert!(line.contains("running"));
        assert!(line.contains("do something"));
    }

    #[test]
    fn latest_job_wins() {
        let agents = vec!["researcher".to_string()];
        let jobs = vec![
            job("job-2", "researcher", JobStatus::Error),
            job("job-10", "researcher", JobStatus::Done),
        ];
        let pane = render(&agents, &jobs).unwrap();
        let line: String = pane.content[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        // job-10 (done) is newer than job-2 (error) by numeric id.
        assert!(line.contains("idle"), "expected idle from job-10: {line}");
        assert!(!line.contains("error"));
    }
}

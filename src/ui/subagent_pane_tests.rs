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
    assert_eq!(pane.content.len(), 3); // 2 agents + separator line
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
fn idle_agent_is_greyed_out() {
    let agents = vec!["researcher".to_string()];
    let pane = render(&agents, &[]).unwrap();
    // Icon span and name span should both be DarkGray when idle.
    let spans = &pane.content[0].spans;
    assert_eq!(spans[0].style.fg, Some(Color::DarkGray));
    assert_eq!(spans[1].style.fg, Some(Color::DarkGray));
}

#[test]
fn running_agent_is_not_greyed() {
    let agents = vec!["researcher".to_string()];
    let jobs = vec![job("job-1", "researcher", JobStatus::Running)];
    let pane = render(&agents, &jobs).unwrap();
    let spans = &pane.content[0].spans;
    assert_eq!(spans[0].style.fg, Some(Color::White));
    assert_eq!(spans[1].style.fg, Some(Color::White));
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

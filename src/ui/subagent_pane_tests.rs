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
        max_concurrency: 1,
        cancel_flag: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        parent: None,
    }
}

#[test]
fn render_returns_none_without_agents() {
    assert!(render(&[], &[]).is_none());
}

#[test]
fn render_includes_ad_hoc_job_agents() {
    let jobs = vec![job("job-1", "shotgun codex/gpt-5 #1", JobStatus::Running)];
    let pane = render(&[], &jobs).unwrap();
    let first: String = pane.content[0]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    assert!(first.contains("shotgun codex/gpt-5 #1"));
    assert!(first.contains("running"));
}

#[test]
fn render_lists_all_agents() {
    let agents = vec!["researcher".to_string(), "coder".to_string()];
    let jobs = vec![
        job("job-1", "researcher", JobStatus::Running),
        job("job-2", "coder", JobStatus::Running),
    ];
    let pane = render(&agents, &jobs).unwrap();
    assert_eq!(pane.source, PANE_SOURCE);
    assert_eq!(pane.title, "Agents (2)");
    assert_eq!(pane.content.len(), 3); // 2 agents + separator line
    // Running agents show the running marker.
    let first: String = pane.content[0]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    assert!(first.contains("◑"), "expected running icon: {first}");
    assert!(first.contains("researcher"));
    assert!(first.contains("running"));
}

#[test]
fn render_returns_none_when_all_idle() {
    let agents = vec!["researcher".to_string(), "coder".to_string()];
    assert!(render(&agents, &[]).is_none());
}

#[test]
fn render_returns_none_when_jobs_done() {
    let agents = vec!["researcher".to_string()];
    let jobs = vec![job("job-10", "researcher", JobStatus::Done)];
    assert!(render(&agents, &jobs).is_none());
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
fn running_agent_is_not_greyed() {
    let agents = vec!["researcher".to_string()];
    let jobs = vec![job("job-1", "researcher", JobStatus::Running)];
    let pane = render(&agents, &jobs).unwrap();
    let spans = &pane.content[0].spans;
    assert_eq!(spans[0].style.fg, Some(Color::White));
    assert_eq!(spans[1].style.fg, Some(Color::White));
}

#[test]
fn multi_job_shows_header() {
    let agents = vec!["researcher".to_string()];
    let jobs = vec![
        job("job-1", "researcher", JobStatus::Running),
        job("job-2", "researcher", JobStatus::Running),
    ];
    let pane = render(&agents, &jobs).unwrap();
    let header: String = pane.content[0]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    assert!(header.contains("researcher"));
    assert!(header.contains("2 running"));
}

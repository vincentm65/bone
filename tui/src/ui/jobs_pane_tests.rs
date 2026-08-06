use super::*;

fn job(id: &str, agent: &str, status: JobStatus) -> Job {
    Job {
        id: id.to_string(),
        agent: agent.to_string(),
        task: "do something".to_string(),
        title: String::new(),
        status,
        started_at: current_unix_seconds(),
        token_sent: 0,
        token_received: 0,
        provider: "test-provider".into(),
        activity: None,
        events: Vec::new(),
    }
}

fn theme() -> Theme {
    Theme::default()
}

#[test]
fn render_returns_none_without_agents() {
    assert!(render(&theme(), &[]).is_none());
}

#[test]
fn render_includes_ad_hoc_job_agents() {
    let jobs = vec![job("job-1", "shotgun codex/gpt-5 #1", JobStatus::Running)];
    let pane = render(&theme(), &jobs).unwrap();
    let first: String = pane.content[0]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    assert!(first.contains("shotgun codex/gpt-5 #1"));
    assert!(!first.contains("running"));
}

#[test]
fn render_lists_all_agents() {
    let jobs = vec![
        job("job-1", "researcher", JobStatus::Running),
        job("job-2", "coder", JobStatus::Running),
    ];
    let pane = render(&theme(), &jobs).unwrap();
    assert_eq!(pane.source, PANE_SOURCE);
    assert_eq!(pane.title, "Agents (2)");
    assert_eq!(pane.content.len(), 3); // 2 agents + separator line
    // Running agents show the active marker.
    let first: String = pane.content[0]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    assert!(first.contains("◑"), "expected running icon: {first}");
    assert!(first.contains("researcher"));
    assert!(!first.contains("running"));
}

#[test]
fn render_shows_queued_status() {
    let jobs = vec![job("job-10", "researcher", JobStatus::Queued)];
    let pane = render(&theme(), &jobs).unwrap();
    let line: String = pane.content[0]
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    assert!(line.contains("⧗"), "expected queued icon: {line}");
    assert!(line.contains("queued"));
}

#[test]
fn render_shows_running_status() {
    let jobs = vec![job("job-1", "researcher", JobStatus::Running)];
    let pane = render(&theme(), &jobs).unwrap();
    let line: String = pane.content[0]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    assert!(line.contains("◑"), "expected running icon: {line}");
    assert!(!line.contains("running"));
    assert!(line.contains("do something"));
}

#[test]
fn selected_job_is_marked() {
    let jobs = vec![job("job-1", "researcher", JobStatus::Running)];
    let mut theme = theme();
    theme.palette.selection = Color::Blue;
    let pane = render_selected(&theme, &jobs, Some("job-1")).unwrap();
    let line: String = pane.content[0]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    assert!(line.contains('›'));
    assert_eq!(pane.content[0].style.bg, Some(theme.palette.selection));
}

#[test]
fn selected_job_scrolls_into_view() {
    let jobs: Vec<_> = (0..10)
        .map(|i| {
            job(
                &format!("job-{i}"),
                &format!("agent-{i}"),
                JobStatus::Running,
            )
        })
        .collect();
    let pane = render_selected(&theme(), &jobs, Some("job-9")).unwrap();

    assert_eq!(pane.scroll, 2);
    assert!(pane.scroll <= pane.max_scroll());
}

#[test]
fn running_agent_is_not_greyed() {
    let jobs = vec![job("job-1", "researcher", JobStatus::Running)];
    let theme = theme();
    let pane = render(&theme, &jobs).unwrap();
    let spans = &pane.content[0].spans;
    assert_eq!(spans[0].style.fg, Some(theme.palette.accent));
    assert_eq!(spans[1].style.fg, Some(theme.palette.fg));
}

#[test]
fn multi_job_shows_header() {
    let jobs = vec![
        job("job-1", "researcher", JobStatus::Running),
        job("job-2", "researcher", JobStatus::Running),
    ];
    let pane = render(&theme(), &jobs).unwrap();
    let header: String = pane.content[0]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    assert!(header.contains("researcher"));
    assert!(header.contains("2 active"));
}

#[test]
fn format_tokens_small() {
    assert_eq!(format_tokens(0), "0");
    assert_eq!(format_tokens(999), "999");
}

#[test]
fn format_tokens_thousands() {
    assert_eq!(format_tokens(1_000), "1,000");
    assert_eq!(format_tokens(1_992), "1,992");
    assert_eq!(format_tokens(9_999), "9,999");
    assert_eq!(format_tokens(10_000), "10.0k");
}

#[test]
fn format_tokens_k() {
    assert_eq!(format_tokens(10_001), "10.0k");
    assert_eq!(format_tokens(44_400), "44.4k");
    assert_eq!(format_tokens(999_999), "1000.0k");
}

#[test]
fn format_tokens_m() {
    assert_eq!(format_tokens(1_000_000), "1.00m");
    assert_eq!(format_tokens(1_234_567), "1.23m");
}

use super::*;

fn process(owner: &str, running: bool) -> Process {
    Process {
        snapshot: ProcessSnapshot {
            id: String::new(),
            command: String::new(),
            owner: owner.into(),
            running,
            state: if running {
                ProcessState::Running
            } else {
                ProcessState::Exited
            },
            started_at: 0,
            finished_at: None,
            stdout: String::new(),
            stderr: String::new(),
            exit_code: None,
            signal: None,
            error: None,
        },
        cancel: Arc::new(AtomicBool::new(false)),
        order: 0,
    }
}

#[test]
fn live_output_is_bounded_and_keeps_the_latest_text() {
    let mut output = String::new();
    append_bounded(&mut output, &vec![b'a'; LIVE_OUTPUT_BYTES]);
    append_bounded(&mut output, "latest λ".as_bytes());

    assert!(output.len() <= LIVE_OUTPUT_BYTES);
    assert!(output.starts_with(LIVE_OUTPUT_MARKER));
    assert!(output.ends_with("latest λ"));
}

#[test]
fn process_list_is_newest_first_and_scoped() {
    let mut oldest = process("conversation:7", true);
    oldest.order = 1;
    oldest.snapshot.id = "oldest".into();
    let mut newest = process("conversation:7", true);
    newest.order = 3;
    newest.snapshot.id = "newest".into();
    let mut foreign = process("conversation:8", true);
    foreign.order = 4;
    foreign.snapshot.id = "foreign".into();
    let registry = ProcessRegistry {
        next: AtomicU64::new(5),
        version: AtomicU64::new(1),
        processes: Mutex::new(HashMap::from([
            ("oldest".into(), oldest),
            ("newest".into(), newest),
            ("foreign".into(), foreign),
        ])),
    };

    let ids: Vec<_> = registry
        .list(Some("conversation:7"))
        .into_iter()
        .map(|process| process.id)
        .collect();

    assert_eq!(ids, ["newest", "oldest"]);
}

#[test]
fn completed_processes_are_bounded_without_evicting_running_processes() {
    let mut processes = HashMap::new();
    for order in 0..=MAX_COMPLETED_PROCESSES as u64 {
        let id = format!("process-{order}");
        let mut entry = process("conversation:7", false);
        entry.snapshot.id = id.clone();
        entry.order = order;
        processes.insert(id, entry);
    }
    let mut running = process("conversation:7", true);
    running.snapshot.id = "running".into();
    processes.insert("running".into(), running);

    ProcessRegistry::prune_completed(&mut processes);

    assert_eq!(processes.len(), MAX_COMPLETED_PROCESSES + 1);
    assert!(!processes.contains_key("process-0"));
    assert!(processes.contains_key("running"));
}

#[test]
fn kill_all_scoped_only_cancels_running_processes_in_scope() {
    let registry = ProcessRegistry {
        next: AtomicU64::new(1),
        version: AtomicU64::new(1),
        processes: Mutex::new(HashMap::from([
            ("owned-running".into(), process("conversation:7", true)),
            ("owned-finished".into(), process("conversation:7", false)),
            ("other-running".into(), process("conversation:8", true)),
        ])),
    };

    assert_eq!(registry.kill_all_scoped("conversation:7"), 1);
    let processes = registry.processes.lock().unwrap();
    assert!(processes["owned-running"].cancel.load(Ordering::Relaxed));
    assert!(!processes["owned-finished"].cancel.load(Ordering::Relaxed));
    assert!(!processes["other-running"].cancel.load(Ordering::Relaxed));
}

#[test]
fn clear_completed_scoped_removes_only_finished_processes_in_scope() {
    let registry = ProcessRegistry {
        next: AtomicU64::new(4),
        version: AtomicU64::new(1),
        processes: Mutex::new(HashMap::from([
            ("owned-finished".into(), process("conversation:7", false)),
            ("owned-running".into(), process("conversation:7", true)),
            ("other-finished".into(), process("conversation:8", false)),
        ])),
    };

    assert_eq!(registry.clear_completed_scoped("conversation:7"), 1);
    let processes = registry.processes.lock().unwrap();
    assert!(!processes.contains_key("owned-finished"));
    assert!(processes.contains_key("owned-running"));
    assert!(processes.contains_key("other-finished"));
}

#[tokio::test]
async fn records_exit_timeout_cancel_and_timestamps() {
    let registry = registry();
    let owner = format!("process-test:{}", now_millis());
    let exited = registry.spawn("printf normal".into(), owner.clone(), 5_000, None);
    let nonzero = registry.spawn("printf failed; exit 7".into(), owner.clone(), 5_000, None);
    let timed_out = registry.spawn("sleep 2".into(), owner.clone(), 1_000, None);
    let cancelled = registry.spawn("printf partial; sleep 2".into(), owner, 5_000, None);

    while [
        exited.as_str(),
        nonzero.as_str(),
        timed_out.as_str(),
        cancelled.as_str(),
    ]
    .iter()
    .any(|id| registry.get(id).is_some_and(|process| process.running))
    {
        if registry
            .get(&cancelled)
            .is_some_and(|process| process.stdout.contains("partial"))
        {
            let _ = registry.kill(&cancelled);
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let exited_snapshot = registry.get(&exited).unwrap();
    assert_eq!(exited_snapshot.state, ProcessState::Exited);
    assert_eq!(exited_snapshot.stdout, "normal");
    assert!(exited_snapshot.finished_at.unwrap() >= exited_snapshot.started_at);

    let nonzero_snapshot = registry.get(&nonzero).unwrap();
    assert_eq!(nonzero_snapshot.state, ProcessState::Exited);
    assert_eq!(nonzero_snapshot.exit_code, Some(7));
    assert_eq!(
        registry.get(&timed_out).unwrap().state,
        ProcessState::TimedOut
    );

    let cancelled_snapshot = registry.get(&cancelled).unwrap();
    assert_eq!(cancelled_snapshot.state, ProcessState::Cancelled);
    assert!(cancelled_snapshot.stdout.contains("partial"));
}

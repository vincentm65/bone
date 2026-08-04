use super::*;

fn process(owner: &str, running: bool) -> Process {
    Process {
        snapshot: ProcessSnapshot {
            id: String::new(),
            command: String::new(),
            owner: owner.into(),
            running,
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

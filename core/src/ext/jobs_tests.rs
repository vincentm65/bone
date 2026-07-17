use super::*;

fn fresh_registry() -> JobRegistry {
    JobRegistry::new()
}

/// A `NewJob` with default cap (1) and a fresh cancel flag.
fn new_job(agent: &str, task: &str) -> NewJob {
    NewJob {
        agent: agent.to_string(),
        task: task.to_string(),
        title: String::new(),
        max_concurrency: 1,
        scope: None,
        cancel_flag: Arc::new(AtomicBool::new(false)),
    }
}

fn create_default(reg: &JobRegistry, agent: &str, task: &str) -> String {
    reg.create(new_job(agent, task))
}

#[test]
fn create_and_snapshot() {
    let reg = fresh_registry();
    let id = create_default(&reg, "researcher", "search the web");
    assert_eq!(id, "job-1");
    let snap = reg.snapshot();
    let arr = snap.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    let job = &arr[0];
    assert_eq!(job["id"], "job-1");
    assert_eq!(job["agent"], "researcher");
    assert_eq!(job["status"], "running");
    assert!(job["result"].is_null());
}

#[test]
fn create_queues_at_concurrency_cap() {
    let reg = fresh_registry();
    let id1 = create_default(&reg, "coder", "task one");
    let id2 = reg.create(new_job("coder", "task two"));
    assert_eq!(
        reg.all_jobs()
            .into_iter()
            .find(|j| j.id == id2)
            .unwrap()
            .status,
        JobStatus::Queued
    );
    assert!(!reg.try_start(&id2));

    // Once the first job finishes, the agent is free again.
    reg.complete(&id1, Ok("done".into()));
    assert!(reg.try_start(&id2));
}

#[test]
fn create_respects_concurrency_cap() {
    let reg = fresh_registry();
    // max_concurrency=2 allows two jobs.
    create_default(&reg, "parallel", "task one");
    reg.create(NewJob {
        max_concurrency: 2,
        ..new_job("parallel", "task two")
    });
    let third = reg.create(NewJob {
        max_concurrency: 2,
        ..new_job("parallel", "task three")
    });
    assert_eq!(
        reg.all_jobs()
            .into_iter()
            .find(|j| j.id == third)
            .unwrap()
            .status,
        JobStatus::Queued
    );
}

#[test]
fn create_allows_different_agents_concurrently() {
    let reg = fresh_registry();
    create_default(&reg, "a", "t1");
    assert_eq!(reg.all_jobs().last().unwrap().status, JobStatus::Running);
}

#[test]
fn cancel_sets_flag_and_completes() {
    let reg = fresh_registry();
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let id = reg.create(NewJob {
        cancel_flag: cancel_flag.clone(),
        ..new_job("cancellable", "task")
    });

    // Cancel the job.
    assert!(reg.cancel(&id));
    assert!(cancel_flag.load(Ordering::Relaxed));

    // Cancelling a non-existent job returns false.
    assert!(!reg.cancel("nonexistent"));
}

#[test]
fn complete_sets_done() {
    let reg = fresh_registry();
    create_default(&reg, "coder", "write code");
    reg.complete("job-1", Ok("finished".into()));
    let snap = reg.snapshot();
    assert_eq!(snap[0]["status"], "done");
    assert_eq!(snap[0]["result"], "finished");
    assert!(snap[0]["finished_at"].is_number());
    assert!(snap[0]["result_file"].is_null());
}

#[test]
fn complete_sets_error() {
    let reg = fresh_registry();
    create_default(&reg, "coder", "write code");
    reg.complete("job-1", Err("boom".into()));
    let snap = reg.snapshot();
    assert_eq!(snap[0]["status"], "error");
    assert_eq!(snap[0]["result"], "boom");
}

/// A job created with a `scope` is only cancelled / peeked by the matching
/// scope — the guard that keeps a multi-conversation `bone serve` from
/// injecting or killing another conversation's sub-agents.
#[test]
fn scoped_cancel_and_peek_isolate_conversations() {
    let reg = fresh_registry();
    let flag_a = Arc::new(AtomicBool::new(false));
    let flag_b = Arc::new(AtomicBool::new(false));
    let a = reg.create(NewJob {
        scope: Some(1),
        cancel_flag: flag_a.clone(),
        ..new_job("a", "conv-1 work")
    });
    let b = reg.create(NewJob {
        scope: Some(2),
        cancel_flag: flag_b.clone(),
        ..new_job("b", "conv-2 work")
    });

    // Cancelling conversation 1 leaves conversation 2's job untouched.
    assert_eq!(reg.cancel_all_scoped(Some(1)), 1);
    assert!(flag_a.load(Ordering::Relaxed));
    assert!(!flag_b.load(Ordering::Relaxed));

    // Finished results are only visible to their own scope.
    reg.complete(&a, Ok("a done".into()));
    reg.complete(&b, Ok("b done".into()));
    let for_1 = reg.peek_finished_unconsumed_scoped(Some(1));
    assert_eq!(for_1.len(), 1);
    assert_eq!(for_1[0].id, a);
    let for_2 = reg.peek_finished_unconsumed_scoped(Some(2));
    assert_eq!(for_2.len(), 1);
    assert_eq!(for_2[0].id, b);
    // A scope with no jobs sees nothing.
    assert!(reg.peek_finished_unconsumed_scoped(Some(99)).is_empty());
}

#[test]
fn transcript_of_respects_scope() {
    let reg = fresh_registry();
    let id = reg.create(NewJob {
        scope: Some(1),
        ..new_job("researcher", "conv-1 work")
    });
    let transcript = vec![crate::llm::ChatMessage::new(
        crate::llm::ChatRole::Assistant,
        "done",
    )];
    reg.complete_with_tokens(&id, Ok("done".into()), 0, 0, Some(transcript.clone()));

    let saved = reg.transcript_of(&id, Some(1)).expect("matching scope");
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].content, "done");
    assert!(reg.transcript_of(&id, Some(2)).is_none());
}

#[test]
fn format_results_for_injection_batches_and_notes_running() {
    let reg = fresh_registry();
    let done = reg.create(NewJob {
        scope: Some(7),
        ..new_job("researcher", "search")
    });
    reg.complete(&done, Ok("found it".into()));
    reg.create(NewJob {
        scope: Some(7),
        ..new_job("watcher", "keep watching")
    });

    let finished = reg.peek_finished_unconsumed_scoped(Some(7));
    let running = reg.running_jobs_scoped(Some(7));
    let (turn_text, display_text) =
        format_results_for_injection(&finished, &running).expect("finished job present");

    assert!(turn_text.contains("found it"));
    assert!(turn_text.contains(&done)); // job id appears in the batch header
    assert!(turn_text.contains("still running")); // the outstanding watcher is noted
    assert!(display_text.starts_with("[job results:"));

    // No finished jobs → nothing to inject.
    assert!(format_results_for_injection(&[], &running).is_none());
}

#[test]
fn complete_spills_long_result_to_file() {
    let reg = fresh_registry();
    let id = create_default(&reg, "coder", "long output");
    let long = "x".repeat(MAX_INJECT_CHARS + 100);
    reg.complete(&id, Ok(long.clone()));
    let snap = reg.snapshot();
    let path = snap[0]["result_file"].as_str().expect("spill file path");
    let on_disk = std::fs::read_to_string(path).unwrap();
    assert_eq!(on_disk, long);
    let _ = std::fs::remove_file(path);
}

#[test]
fn version_bumps_on_mutation() {
    let reg = fresh_registry();
    assert_eq!(reg.version(), 0);
    create_default(&reg, "a", "t");
    assert_eq!(reg.version(), 1);
    reg.complete("job-1", Ok("ok".into()));
    assert_eq!(reg.version(), 2);
}

#[test]
fn update_tokens_bumps_version_on_change() {
    let reg = fresh_registry();
    let id = create_default(&reg, "a", "t");
    let v = reg.version();
    reg.update_tokens(&id, 10, 20);
    assert_eq!(reg.version(), v + 1);
    // No change → no bump.
    reg.update_tokens(&id, 10, 20);
    assert_eq!(reg.version(), v + 1);
}

#[test]
fn cancel_bumps_version() {
    let reg = fresh_registry();
    let id = reg.create(new_job("a", "t"));
    let v = reg.version();
    assert!(reg.cancel(&id));
    assert_eq!(reg.version(), v + 1);
    // Cancelling again while still running returns true, bumps version.
    assert!(reg.cancel(&id));
    assert_eq!(reg.version(), v + 2);
    // After completion, cancelling returns false and does not bump version.
    reg.complete(&id, Ok("done".into()));
    assert!(!reg.cancel(&id));
    assert_eq!(reg.version(), v + 3);
}

#[test]
fn peek_then_mark_consumed() {
    let reg = fresh_registry();
    create_default(&reg, "a", "t");
    reg.complete("job-1", Ok("result".into()));

    // Peek does not consume.
    let peeked = reg.peek_finished_unconsumed();
    assert_eq!(peeked.len(), 1);
    assert_eq!(peeked[0].id, "job-1");
    assert_eq!(reg.peek_finished_unconsumed().len(), 1);

    // Marking consumes and bumps version.
    let v = reg.version();
    reg.mark_consumed(&["job-1".into()]);
    assert_eq!(reg.version(), v + 1);
    assert!(reg.peek_finished_unconsumed().is_empty());

    // Marking again is a no-op.
    reg.mark_consumed(&["job-1".into()]);
    assert_eq!(reg.version(), v + 1);
}

#[test]
fn peek_skips_running() {
    let reg = fresh_registry();
    create_default(&reg, "a", "t");
    // Job is still running.
    assert!(reg.peek_finished_unconsumed().is_empty());
    assert_eq!(reg.version(), 1);
}

#[test]
fn pruning_caps_registry_size() {
    let reg = fresh_registry();
    for i in 0..(MAX_RETAINED_JOBS + 10) {
        let id = reg.create(new_job(&format!("agent-{i}"), "t"));
        reg.complete(&id, Ok("r".into()));
        reg.mark_consumed(std::slice::from_ref(&id));
    }
    let len = reg.snapshot().as_array().unwrap().len();
    assert!(len <= MAX_RETAINED_JOBS, "registry not pruned: {len}");
}

#[test]
fn pruning_keeps_unconsumed_jobs() {
    let reg = fresh_registry();
    // First job finishes but is never consumed.
    let keep = create_default(&reg, "keeper", "t");
    reg.complete(&keep, Ok("important".into()));
    for i in 0..(MAX_RETAINED_JOBS + 10) {
        let id = reg.create(new_job(&format!("agent-{i}"), "t"));
        reg.complete(&id, Ok("r".into()));
        reg.mark_consumed(std::slice::from_ref(&id));
    }
    let peeked = reg.peek_finished_unconsumed();
    assert!(peeked.iter().any(|j| j.id == keep));
}

#[test]
fn note_event_bumps_version_only_for_known_job() {
    let reg = fresh_registry();
    let id = create_default(&reg, "a", "t");
    let version = reg.version();
    reg.note_event(
        &id,
        crate::runtime::RuntimeEvent::TextDelta { text: "x".into() },
        None,
    );
    assert_eq!(reg.version(), version + 1);
    assert_eq!(reg.all_jobs()[0].events.len(), 1);

    let version = reg.version();
    reg.note_event(
        "unknown",
        crate::runtime::RuntimeEvent::TextDelta { text: "x".into() },
        None,
    );
    assert_eq!(reg.version(), version);
}

#[test]
fn truncate_for_injection_respects_boundary() {
    let long = "a".repeat(20_000);
    let truncated = truncate_for_injection(&long, MAX_INJECT_CHARS);
    assert!(truncated.len() < 20_000);
    assert!(truncated.ends_with(TRUNCATION_MARKER));
}

#[test]
fn truncate_for_injection_handles_multibyte_cutoff() {
    let truncated = truncate_for_injection("éééééé", 5);
    assert_eq!(truncated, format!("ééééé{TRUNCATION_MARKER}"));
}

#[test]
fn truncate_for_injection_word_boundary() {
    let s = "hello world foo bar baz";
    let truncated = truncate_for_injection(s, 15);
    assert!(truncated.ends_with(TRUNCATION_MARKER));
    // Should break at a word boundary, not in the middle of a word.
    let kept = truncated.trim_end_matches(TRUNCATION_MARKER);
    assert!(s.split(' ').any(|w| kept.ends_with(w)), "kept: {kept}");
}

#[test]
fn truncate_for_injection_ignores_distant_boundary() {
    let max_chars = 1_000;
    let source = format!("prefix {}", "x".repeat(max_chars * 2));
    let truncated = truncate_for_injection(&source, max_chars);
    let kept = truncated.trim_end_matches(TRUNCATION_MARKER);
    assert!(
        kept.chars().count() >= 900,
        "kept only {} chars",
        kept.chars().count()
    );
}

#[test]
fn truncate_short_string_unchanged() {
    assert_eq!(truncate_for_injection("hi", 100), "hi");
}

#[test]
fn wait_for_returns_immediately_when_finished() {
    let reg = fresh_registry();
    let id = create_default(&reg, "a", "t");
    reg.complete(&id, Ok("result".into()));

    let outcome = reg.wait_for(std::slice::from_ref(&id), Duration::from_secs(5), None);
    assert_eq!(outcome.finished.len(), 1);
    assert_eq!(outcome.finished[0].id, id);
    assert!(outcome.pending.is_empty());
    assert!(!outcome.timed_out);
    assert!(!outcome.cancelled);

    // Waited jobs are consumed: not re-delivered via auto-injection.
    assert!(reg.peek_finished_unconsumed().is_empty());
}

#[test]
fn wait_for_times_out_on_running_job() {
    let reg = fresh_registry();
    let id = create_default(&reg, "a", "t");

    let outcome = reg.wait_for(std::slice::from_ref(&id), Duration::from_millis(50), None);
    assert!(outcome.finished.is_empty());
    assert_eq!(outcome.pending, vec![id.clone()]);
    assert!(outcome.timed_out);

    // Job stays unconsumed for later auto-injection.
    reg.complete(&id, Ok("late".into()));
    assert_eq!(reg.peek_finished_unconsumed().len(), 1);
}

#[test]
fn wait_for_wakes_on_completion() {
    let reg = std::sync::Arc::new(fresh_registry());
    let id = create_default(&reg, "a", "t");

    let reg2 = reg.clone();
    let id2 = id.clone();
    let handle = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        reg2.complete(&id2, Ok("done".into()));
    });

    let outcome = reg.wait_for(&[id], Duration::from_secs(5), None);
    handle.join().unwrap();
    assert_eq!(outcome.finished.len(), 1);
    assert_eq!(outcome.finished[0].result.as_deref(), Some("done"));
    assert!(!outcome.timed_out);
}

#[test]
fn wait_for_respects_cancellation() {
    let reg = fresh_registry();
    let id = create_default(&reg, "a", "t");
    let cancelled = AtomicBool::new(true);

    let outcome = reg.wait_for(
        std::slice::from_ref(&id),
        Duration::from_secs(5),
        Some(&cancelled),
    );
    assert!(outcome.cancelled);
    assert!(!outcome.timed_out);
    assert_eq!(outcome.pending, vec![id]);
}

#[test]
fn wait_for_ignores_unknown_ids() {
    let reg = fresh_registry();
    let outcome = reg.wait_for(&["job-999".into()], Duration::from_secs(5), None);
    assert!(outcome.finished.is_empty());
    assert!(outcome.pending.is_empty());
    assert!(!outcome.timed_out);
}

#[test]
fn wait_for_returns_already_consumed_jobs() {
    let reg = fresh_registry();
    let id = create_default(&reg, "a", "t");
    reg.complete(&id, Ok("r".into()));
    reg.mark_consumed(std::slice::from_ref(&id));

    let outcome = reg.wait_for(&[id], Duration::from_secs(5), None);
    assert_eq!(outcome.finished.len(), 1);
}

#[test]
fn running_ids_lists_only_running() {
    let reg = fresh_registry();
    let id1 = create_default(&reg, "a", "t1");
    let id2 = create_default(&reg, "b", "t2");
    reg.complete(&id1, Ok("done".into()));
    assert_eq!(reg.running_ids(), vec![id2.clone()]);
    let running = reg.running_jobs();
    assert_eq!(running.len(), 1);
    assert_eq!(running[0].id, id2);
    assert_eq!(running[0].agent, "b");
}

#[test]
fn multiple_jobs_ordered_in_snapshot() {
    let reg = fresh_registry();
    create_default(&reg, "a", "t1");
    create_default(&reg, "b", "t2");
    let snap = reg.snapshot();
    assert_eq!(snap[0]["agent"], "a");
    assert_eq!(snap[1]["agent"], "b");
}

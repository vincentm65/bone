//! Job registry — background sub-agent task management.
//!
//! Jobs are created by `ctx.agent.spawn`, run as detached tokio tasks, and
//! their results are queryable via `ctx.agent.jobs` or consumed via
//! `take_finished_unconsumed` (for auto-injection).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;

/// Maximum characters per job result at injection time.
pub const MAX_INJECT_CHARS: usize = 16_000;

/// Maximum jobs retained in the registry. When exceeded, the oldest
/// finished-and-consumed jobs are pruned.
const MAX_RETAINED_JOBS: usize = 200;

// ── Public types ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Running,
    Done,
    Error,
}

impl JobStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            JobStatus::Running => "running",
            JobStatus::Done => "done",
            JobStatus::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Job {
    pub id: String,
    pub agent: String,
    pub task: String,
    pub status: JobStatus,
    pub result: Option<String>,
    pub started_at: u64,
    pub finished_at: Option<u64>,
    pub consumed: bool,
    pub token_sent: u64,
    pub token_received: u64,
    /// Path to a file holding the full result when it exceeds the
    /// injection limit (results are truncated when delivered inline).
    pub result_file: Option<String>,
}

// ── Registry ────────────────────────────────────────────────────────────────

pub struct JobRegistry {
    jobs: Mutex<Vec<Job>>,
    /// Notified whenever a job completes, so `wait_for` can wake up.
    completed: Condvar,
    version: AtomicU64,
    next_id: AtomicU64,
}

/// Outcome of a blocking [`JobRegistry::wait_for`] call.
#[derive(Debug)]
pub struct WaitOutcome {
    /// Jobs (among the requested ids) that finished. Marked consumed.
    pub finished: Vec<Job>,
    /// Requested ids still running when the wait ended (timeout/cancel).
    pub pending: Vec<String>,
    /// The wait was interrupted by the cancellation flag.
    pub cancelled: bool,
    /// The wait hit its deadline with jobs still pending.
    pub timed_out: bool,
}

impl Default for JobRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl JobRegistry {
    pub fn new() -> Self {
        Self {
            jobs: Mutex::new(Vec::new()),
            completed: Condvar::new(),
            version: AtomicU64::new(0),
            next_id: AtomicU64::new(1),
        }
    }

    /// Monotonic version counter, bumped on every mutation.
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Relaxed)
    }

    /// Create a new running job. Returns its ID, or an error if the named
    /// agent already has a running job (checked atomically under the lock).
    pub fn create(&self, agent: String, task: String) -> Result<String, String> {
        let now = current_unix_seconds();
        let mut jobs = self.jobs.lock().unwrap();
        if let Some(busy) = jobs
            .iter()
            .find(|j| j.status == JobStatus::Running && j.agent == agent)
        {
            return Err(format!(
                "agent '{}' is already busy with {}; wait for it to finish",
                agent, busy.id
            ));
        }
        let id = format!("job-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        jobs.push(Job {
            id: id.clone(),
            agent,
            task,
            status: JobStatus::Running,
            result: None,
            started_at: now,
            finished_at: None,
            consumed: false,
            token_sent: 0,
            token_received: 0,
            result_file: None,
        });
        // Prune oldest finished-and-consumed jobs to cap memory growth.
        if jobs.len() > MAX_RETAINED_JOBS {
            let excess = jobs.len() - MAX_RETAINED_JOBS;
            let mut removed = 0;
            jobs.retain(|j| {
                if removed < excess && j.status != JobStatus::Running && j.consumed {
                    removed += 1;
                    false
                } else {
                    true
                }
            });
        }
        drop(jobs);
        self.version.fetch_add(1, Ordering::Relaxed);
        Ok(id)
    }

    /// Mark a job as finished (Ok or Error).
    pub fn complete(&self, id: &str, result: Result<String, String>) {
        let now = current_unix_seconds();
        let status = if result.is_ok() {
            JobStatus::Done
        } else {
            JobStatus::Error
        };
        let result = result.unwrap_or_else(|e| e);
        let result_file = spill_result(id, &result);
        let mut jobs = self.jobs.lock().unwrap();
        if let Some(job) = jobs.iter_mut().find(|j| j.id == id) {
            job.status = status;
            job.result = Some(result);
            job.finished_at = Some(now);
            job.result_file = result_file;
        }
        self.completed.notify_all();
        drop(jobs);
        self.version.fetch_add(1, Ordering::Relaxed);
    }

    /// Update token counts for a running job.
    pub fn update_tokens(&self, id: &str, sent: u64, received: u64) {
        let mut jobs = self.jobs.lock().unwrap();
        if let Some(job) = jobs.iter_mut().find(|j| j.id == id)
            && (job.token_sent != sent || job.token_received != received)
        {
            job.token_sent = sent;
            job.token_received = received;
            drop(jobs);
            self.version.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Update token counts from shared atomics when a job completes.
    pub fn complete_with_tokens(
        &self,
        id: &str,
        result: Result<String, String>,
        token_sent: u64,
        token_received: u64,
    ) {
        let now = current_unix_seconds();
        let status = if result.is_ok() {
            JobStatus::Done
        } else {
            JobStatus::Error
        };
        let result = result.unwrap_or_else(|e| e);
        let result_file = spill_result(id, &result);
        let mut jobs = self.jobs.lock().unwrap();
        if let Some(job) = jobs.iter_mut().find(|j| j.id == id) {
            job.status = status;
            job.result = Some(result);
            job.finished_at = Some(now);
            job.result_file = result_file;
            job.token_sent = token_sent;
            job.token_received = token_received;
        }
        self.completed.notify_all();
        drop(jobs);
        self.version.fetch_add(1, Ordering::Relaxed);
    }

    /// IDs of all currently running jobs.
    pub fn running_ids(&self) -> Vec<String> {
        let jobs = self.jobs.lock().unwrap();
        jobs.iter()
            .filter(|j| j.status == JobStatus::Running)
            .map(|j| j.id.clone())
            .collect()
    }

    /// Clones of all currently running jobs.
    pub fn running_jobs(&self) -> Vec<Job> {
        let jobs = self.jobs.lock().unwrap();
        jobs.iter()
            .filter(|j| j.status == JobStatus::Running)
            .cloned()
            .collect()
    }

    /// Block until all of the given jobs finish, the timeout elapses, or the
    /// cancellation flag is set. Finished jobs (among `ids`) are returned and
    /// marked consumed so they are not auto-injected again later. IDs unknown
    /// to the registry are ignored. Jobs still running when the wait ends are
    /// reported as `pending` and stay unconsumed (they will be auto-injected
    /// on completion).
    pub fn wait_for(
        &self,
        ids: &[String],
        timeout: Duration,
        cancelled: Option<&AtomicBool>,
    ) -> WaitOutcome {
        let deadline = Instant::now() + timeout;
        let mut jobs = self.jobs.lock().unwrap();
        loop {
            let pending: Vec<String> = jobs
                .iter()
                .filter(|j| j.status == JobStatus::Running && ids.contains(&j.id))
                .map(|j| j.id.clone())
                .collect();
            let was_cancelled = cancelled.map(|c| c.load(Ordering::Relaxed)).unwrap_or(false);
            let deadline_hit = Instant::now() >= deadline;

            if pending.is_empty() || was_cancelled || deadline_hit {
                let mut any_consumed = false;
                let mut finished: Vec<Job> = jobs
                    .iter_mut()
                    .filter(|j| j.status != JobStatus::Running && ids.contains(&j.id))
                    .map(|j| {
                        if !j.consumed {
                            j.consumed = true;
                            any_consumed = true;
                        }
                        j.clone()
                    })
                    .collect();
                finished.sort_by_key(|j| j.started_at);
                drop(jobs);
                if any_consumed {
                    self.version.fetch_add(1, Ordering::Relaxed);
                }
                return WaitOutcome {
                    finished,
                    timed_out: deadline_hit && !pending.is_empty() && !was_cancelled,
                    pending,
                    cancelled: was_cancelled,
                };
            }

            // Short timeout so the cancellation flag is rechecked promptly
            // even without a completion notification.
            let (guard, _) = self
                .completed
                .wait_timeout(jobs, Duration::from_millis(100))
                .unwrap();
            jobs = guard;
        }
    }

    /// Snapshot of all jobs as a JSON array.
    pub fn snapshot(&self) -> serde_json::Value {
        let jobs = self.jobs.lock().unwrap();
        let array: Vec<_> = jobs.iter().cloned().collect();
        serde_json::to_value(array).unwrap_or_else(|_| serde_json::json!([]))
    }

    /// Clones of all jobs (e.g. for the Rust-side pane renderer).
    pub fn all_jobs(&self) -> Vec<Job> {
        self.jobs.lock().unwrap().clone()
    }

    /// Peek at all unconsumed finished jobs without marking them consumed.
    /// Call [`JobRegistry::mark_consumed`] after the results have actually
    /// been delivered (e.g. injected into the conversation).
    pub fn peek_finished_unconsumed(&self) -> Vec<Job> {
        let jobs = self.jobs.lock().unwrap();
        let mut finished: Vec<Job> = jobs
            .iter()
            .filter(|j| {
                (j.status == JobStatus::Done || j.status == JobStatus::Error) && !j.consumed
            })
            .cloned()
            .collect();
        finished.sort_by_key(|j| j.started_at);
        finished
    }

    /// Mark the given job ids as consumed. Bumps the version if any changed.
    pub fn mark_consumed(&self, ids: &[String]) {
        let mut jobs = self.jobs.lock().unwrap();
        let mut any = false;
        for job in jobs.iter_mut() {
            if !job.consumed && ids.contains(&job.id) {
                job.consumed = true;
                any = true;
            }
        }
        drop(jobs);
        if any {
            self.version.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Global singleton — jobs are app-lifetime, user-owned.
pub fn registry() -> &'static JobRegistry {
    static INSTANCE: OnceLock<JobRegistry> = OnceLock::new();
    INSTANCE.get_or_init(JobRegistry::new)
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn current_unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Write a full job result to a spill file when it exceeds the injection
/// limit, so the truncated inline delivery can point at the complete output.
/// Returns the file path on success, None otherwise.
fn spill_result(id: &str, result: &str) -> Option<String> {
    if result.chars().count() <= MAX_INJECT_CHARS {
        return None;
    }
    let dir = std::env::temp_dir().join("bone-jobs");
    if std::fs::create_dir_all(&dir).is_err() {
        return None;
    }
    let path = dir.join(format!("{id}.txt"));
    match std::fs::write(&path, result) {
        Ok(()) => Some(path.to_string_lossy().to_string()),
        Err(_) => None,
    }
}

/// Marker appended to truncated job results.
pub const TRUNCATION_MARKER: &str = "\n[... output truncated ...]";

/// Truncate a string to roughly `max_chars` at a word boundary (or hard
/// cutoff), appending an explicit truncation marker when content was cut.
pub fn truncate_for_injection(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let cutoff_byte = s
        .char_indices()
        .nth(max_chars)
        .map(|(idx, _)| idx)
        .unwrap_or(s.len());
    let truncated = &s[..cutoff_byte];
    // Try to break at a word boundary.
    let kept = match truncated.rfind(' ') {
        Some(space) if space > 0 => &truncated[..space],
        _ => truncated,
    };
    format!("{kept}{TRUNCATION_MARKER}")
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_registry() -> JobRegistry {
        JobRegistry::new()
    }

    #[test]
    fn create_and_snapshot() {
        let reg = fresh_registry();
        let id = reg
            .create("researcher".into(), "search the web".into())
            .unwrap();
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
    fn create_rejects_busy_agent() {
        let reg = fresh_registry();
        let id = reg.create("coder".into(), "task one".into()).unwrap();
        let err = reg.create("coder".into(), "task two".into()).unwrap_err();
        assert!(err.contains("busy"), "unexpected error: {err}");
        assert!(err.contains(&id));

        // Once the first job finishes, the agent is free again.
        reg.complete(&id, Ok("done".into()));
        assert!(reg.create("coder".into(), "task two".into()).is_ok());
    }

    #[test]
    fn create_allows_different_agents_concurrently() {
        let reg = fresh_registry();
        reg.create("a".into(), "t1".into()).unwrap();
        assert!(reg.create("b".into(), "t2".into()).is_ok());
    }

    #[test]
    fn complete_sets_done() {
        let reg = fresh_registry();
        reg.create("coder".into(), "write code".into()).unwrap();
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
        reg.create("coder".into(), "write code".into()).unwrap();
        reg.complete("job-1", Err("boom".into()));
        let snap = reg.snapshot();
        assert_eq!(snap[0]["status"], "error");
        assert_eq!(snap[0]["result"], "boom");
    }

    #[test]
    fn complete_spills_long_result_to_file() {
        let reg = fresh_registry();
        let id = reg.create("coder".into(), "long output".into()).unwrap();
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
        reg.create("a".into(), "t".into()).unwrap();
        assert_eq!(reg.version(), 1);
        reg.complete("job-1", Ok("ok".into()));
        assert_eq!(reg.version(), 2);
    }

    #[test]
    fn update_tokens_bumps_version_on_change() {
        let reg = fresh_registry();
        let id = reg.create("a".into(), "t".into()).unwrap();
        let v = reg.version();
        reg.update_tokens(&id, 10, 20);
        assert_eq!(reg.version(), v + 1);
        // No change → no bump.
        reg.update_tokens(&id, 10, 20);
        assert_eq!(reg.version(), v + 1);
    }

    #[test]
    fn peek_then_mark_consumed() {
        let reg = fresh_registry();
        reg.create("a".into(), "t".into()).unwrap();
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
        reg.create("a".into(), "t".into()).unwrap();
        // Job is still running.
        assert!(reg.peek_finished_unconsumed().is_empty());
        assert_eq!(reg.version(), 1);
    }

    #[test]
    fn pruning_caps_registry_size() {
        let reg = fresh_registry();
        for i in 0..(MAX_RETAINED_JOBS + 10) {
            let id = reg.create(format!("agent-{i}"), "t".into()).unwrap();
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
        let keep = reg.create("keeper".into(), "t".into()).unwrap();
        reg.complete(&keep, Ok("important".into()));
        for i in 0..(MAX_RETAINED_JOBS + 10) {
            let id = reg.create(format!("agent-{i}"), "t".into()).unwrap();
            reg.complete(&id, Ok("r".into()));
            reg.mark_consumed(std::slice::from_ref(&id));
        }
        let peeked = reg.peek_finished_unconsumed();
        assert!(peeked.iter().any(|j| j.id == keep));
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
    fn truncate_short_string_unchanged() {
        assert_eq!(truncate_for_injection("hi", 100), "hi");
    }

    #[test]
    fn wait_for_returns_immediately_when_finished() {
        let reg = fresh_registry();
        let id = reg.create("a".into(), "t".into()).unwrap();
        reg.complete(&id, Ok("result".into()));

        let outcome = reg.wait_for(&[id.clone()], Duration::from_secs(5), None);
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
        let id = reg.create("a".into(), "t".into()).unwrap();

        let outcome = reg.wait_for(&[id.clone()], Duration::from_millis(50), None);
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
        let id = reg.create("a".into(), "t".into()).unwrap();

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
        let id = reg.create("a".into(), "t".into()).unwrap();
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
        // A job consumed elsewhere (e.g. by another wait) is still returned —
        // wait_for reports completion regardless of the consumed flag.
        let reg = fresh_registry();
        let id = reg.create("a".into(), "t".into()).unwrap();
        reg.complete(&id, Ok("r".into()));
        reg.mark_consumed(std::slice::from_ref(&id));

        let outcome = reg.wait_for(&[id], Duration::from_secs(5), None);
        assert_eq!(outcome.finished.len(), 1);
    }

    #[test]
    fn running_ids_lists_only_running() {
        let reg = fresh_registry();
        let id1 = reg.create("a".into(), "t1".into()).unwrap();
        let id2 = reg.create("b".into(), "t2".into()).unwrap();
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
        reg.create("a".into(), "t1".into()).unwrap();
        reg.create("b".into(), "t2".into()).unwrap();
        let snap = reg.snapshot();
        assert_eq!(snap[0]["agent"], "a");
        assert_eq!(snap[1]["agent"], "b");
    }
}

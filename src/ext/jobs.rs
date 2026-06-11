//! Job registry — background sub-agent task management.
//!
//! Jobs are created by `ctx.agent.spawn`, run as detached tokio tasks, and
//! their results are queryable via `ctx.agent.jobs` or consumed via
//! `take_finished_unconsumed` (for auto-injection).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use serde::Serialize;

/// Maximum characters per job result at injection time.
pub const MAX_INJECT_CHARS: usize = 16_000;

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
}

// ── Registry ────────────────────────────────────────────────────────────────

pub struct JobRegistry {
    jobs: Mutex<Vec<Job>>,
    version: AtomicU64,
    next_id: AtomicU64,
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
            version: AtomicU64::new(0),
            next_id: AtomicU64::new(1),
        }
    }

    /// Monotonic version counter, bumped on every mutation.
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Relaxed)
    }

    /// Create a new running job. Returns its ID.
    pub fn create(&self, agent: String, task: String) -> String {
        let id = format!("job-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let now = current_unix_seconds();
        let job = Job {
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
        };
        self.jobs.lock().unwrap().push(job);
        self.version.fetch_add(1, Ordering::Relaxed);
        id
    }

    /// Mark a job as finished (Ok or Error).
    pub fn complete(&self, id: &str, result: Result<String, String>) {
        let now = current_unix_seconds();
        let mut jobs = self.jobs.lock().unwrap();
        if let Some(job) = jobs.iter_mut().find(|j| j.id == id) {
            job.status = if result.is_ok() {
                JobStatus::Done
            } else {
                JobStatus::Error
            };
            job.result = Some(result.unwrap_or_else(|e| e));
            job.finished_at = Some(now);
        }
        drop(jobs);
        self.version.fetch_add(1, Ordering::Relaxed);
    }

    /// Update token counts for a running job.
    pub fn update_tokens(&self, id: &str, sent: u64, received: u64) {
        let mut jobs = self.jobs.lock().unwrap();
        if let Some(job) = jobs.iter_mut().find(|j| j.id == id) {
            job.token_sent = sent;
            job.token_received = received;
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
        let mut jobs = self.jobs.lock().unwrap();
        if let Some(job) = jobs.iter_mut().find(|j| j.id == id) {
            job.status = if result.is_ok() {
                JobStatus::Done
            } else {
                JobStatus::Error
            };
            job.result = Some(result.unwrap_or_else(|e| e));
            job.finished_at = Some(now);
            job.token_sent = token_sent;
            job.token_received = token_received;
        }
        drop(jobs);
        self.version.fetch_add(1, Ordering::Relaxed);
    }

    /// Snapshot of all jobs as a JSON array.
    pub fn snapshot(&self) -> serde_json::Value {
        let jobs = self.jobs.lock().unwrap();
        let array: Vec<_> = jobs.iter().cloned().collect();
        serde_json::to_value(array).unwrap_or_else(|_| serde_json::json!([]))
    }

    /// Take all unconsumed finished jobs, mark them consumed, bump version if any were consumed.
    pub fn take_finished_unconsumed(&self) -> Vec<Job> {
        let mut jobs = self.jobs.lock().unwrap();
        let mut finished: Vec<Job> = jobs
            .iter_mut()
            .filter(|j| {
                (j.status == JobStatus::Done || j.status == JobStatus::Error) && !j.consumed
            })
            .map(|j| {
                j.consumed = true;
                j.clone()
            })
            .collect();
        finished.sort_by_key(|j| j.started_at);
        drop(jobs);
        if !finished.is_empty() {
            self.version.fetch_add(1, Ordering::Relaxed);
        }
        finished
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

/// Truncate a string to `max_chars` at a word boundary (or hard cutoff).
pub fn truncate_for_injection(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    if max_chars == 0 {
        return String::new();
    }
    if max_chars <= 3 {
        return ".".repeat(max_chars);
    }

    let cutoff_chars = max_chars - 3;
    let cutoff_byte = s
        .char_indices()
        .nth(cutoff_chars)
        .map(|(idx, _)| idx)
        .unwrap_or(s.len());
    let truncated = &s[..cutoff_byte];
    // Try to break at a word boundary.
    if let Some(space) = truncated.rfind(' ') {
        format!("{}...", &truncated[..space])
    } else {
        format!("{}...", truncated)
    }
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
        let id = reg.create("researcher".into(), "search the web".into());
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
    fn complete_sets_done() {
        let reg = fresh_registry();
        reg.create("coder".into(), "write code".into());
        reg.complete("job-1", Ok("finished".into()));
        let snap = reg.snapshot();
        assert_eq!(snap[0]["status"], "done");
        assert_eq!(snap[0]["result"], "finished");
        assert!(snap[0]["finished_at"].is_number());
    }

    #[test]
    fn complete_sets_error() {
        let reg = fresh_registry();
        reg.create("coder".into(), "write code".into());
        reg.complete("job-1", Err("boom".into()));
        let snap = reg.snapshot();
        assert_eq!(snap[0]["status"], "error");
        assert_eq!(snap[0]["result"], "boom");
    }

    #[test]
    fn version_bumps_on_mutation() {
        let reg = fresh_registry();
        assert_eq!(reg.version(), 0);
        reg.create("a".into(), "t".into());
        assert_eq!(reg.version(), 1);
        reg.complete("job-1", Ok("ok".into()));
        assert_eq!(reg.version(), 2);
    }

    #[test]
    fn take_finished_unconsumed_returns_once() {
        let reg = fresh_registry();
        reg.create("a".into(), "t".into());
        reg.complete("job-1", Ok("result".into()));

        // First take: returns the job.
        let taken = reg.take_finished_unconsumed();
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].id, "job-1");

        // Second take: empty (marked consumed).
        let taken2 = reg.take_finished_unconsumed();
        assert!(taken2.is_empty());
    }

    #[test]
    fn take_finished_unconsumed_empty_does_not_bump_version() {
        let reg = fresh_registry();
        let before = reg.version();
        let taken = reg.take_finished_unconsumed();
        assert!(taken.is_empty());
        assert_eq!(reg.version(), before);
    }

    #[test]
    fn take_finished_unconsumed_skips_running() {
        let reg = fresh_registry();
        reg.create("a".into(), "t".into());
        // Job is still running.
        let taken = reg.take_finished_unconsumed();
        assert!(taken.is_empty());
        assert_eq!(reg.version(), 1);
    }

    #[test]
    fn truncate_for_injection_respects_boundary() {
        let long = "a".repeat(20_000);
        let truncated = truncate_for_injection(&long, MAX_INJECT_CHARS);
        assert!(truncated.len() < 20_000);
        assert!(truncated.ends_with("..."));
    }

    #[test]
    fn truncate_for_injection_handles_multibyte_cutoff() {
        let truncated = truncate_for_injection("éééééé", 5);
        assert_eq!(truncated, "éé...");
    }

    #[test]
    fn truncate_for_injection_word_boundary() {
        let s = "hello world foo bar baz";
        let truncated = truncate_for_injection(s, 15);
        assert!(truncated.ends_with("..."));
        // Should break at a word boundary, not in the middle of a word.
        assert!(!truncated.ends_with("wo..."));
    }

    #[test]
    fn truncate_short_string_unchanged() {
        assert_eq!(truncate_for_injection("hi", 100), "hi");
    }

    #[test]
    fn multiple_jobs_ordered_in_snapshot() {
        let reg = fresh_registry();
        reg.create("a".into(), "t1".into());
        reg.create("b".into(), "t2".into());
        let snap = reg.snapshot();
        assert_eq!(snap[0]["agent"], "a");
        assert_eq!(snap[1]["agent"], "b");
    }
}

//! Job registry — background sub-agent task management.
//!
//! Jobs are created by `ctx.agent.spawn`, run as detached tokio tasks, and
//! their results are queryable via `ctx.agent.jobs` or delivered via the
//! `peek_finished_unconsumed` / `mark_consumed` auto-injection flow.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
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
    /// Maximum concurrent jobs allowed for this agent template.
    pub max_concurrency: usize,
    /// Per-job cancellation flag, settable by [`JobRegistry::cancel`].
    #[serde(skip)]
    pub cancel_flag: Arc<AtomicBool>,
    /// The spawning scope key (prep for Tier 3 recursion).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
}

/// Parameters for [`JobRegistry::create`]. Bundled into a struct so the call
/// sites stay readable and new fields (e.g. Tier 3 scope) don't grow the
/// positional argument list.
pub struct NewJob {
    /// Agent template name (used for the per-template concurrency count).
    pub agent: String,
    /// The task prompt handed to the agent.
    pub task: String,
    /// How many jobs may run concurrently for this agent template.
    pub max_concurrency: usize,
    /// Spawning scope key (prep for Tier 3 recursion; `None` at depth 0).
    pub parent: Option<String>,
    /// Per-job cancellation flag, shared with the running task.
    pub cancel_flag: Arc<AtomicBool>,
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
    /// agent is at its concurrency cap (checked atomically under the lock).
    pub fn create(&self, job: NewJob) -> Result<String, String> {
        let NewJob {
            agent,
            task,
            max_concurrency,
            parent,
            cancel_flag,
        } = job;
        let now = current_unix_seconds();
        let mut jobs = self.jobs.lock().unwrap();
        let running_for_agent = jobs
            .iter()
            .filter(|j| j.status == JobStatus::Running && j.agent == agent)
            .count();
        if running_for_agent >= max_concurrency {
            return Err(format!(
                "agent '{agent}' is at its concurrency cap ({max_concurrency})"
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
            max_concurrency,
            cancel_flag,
            parent,
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
            let was_cancelled = cancelled
                .map(|c| c.load(Ordering::Relaxed))
                .unwrap_or(false);
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

    /// Cancel a job by setting its cancel flag. Returns `true` if the id was
    /// found. The running task observes the flag and aborts at its next await.
    pub fn cancel(&self, id: &str) -> bool {
        let mut jobs = self.jobs.lock().unwrap();
        let Some(job) = jobs.iter_mut().find(|j| j.id == id) else {
            return false;
        };
        if job.status != JobStatus::Running {
            return false;
        }
        job.cancel_flag.store(true, Ordering::Relaxed);
        self.completed.notify_all();
        drop(jobs);
        self.version.fetch_add(1, Ordering::Relaxed);
        true
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
#[path = "jobs_tests.rs"]
mod jobs_tests;

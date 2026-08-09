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

/// Maximum recent-activity entries retained per job.
const MAX_TRACE_LINES: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    /// Waiting for a concurrency slot on its provider.
    Queued,
    Running,
    Done,
    Error,
}

#[derive(Debug, Clone)]
pub struct JobEvent {
    pub event: crate::runtime::RuntimeEvent,
    /// Unified diff captured before an `edit_file` call executes.
    pub edit_preview: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Job {
    pub id: String,
    pub agent: String,
    pub task: String,
    /// Short human-readable summary for display (live pane + tool-call row).
    /// Falls back to a truncation of `task` when empty.
    pub title: String,
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
    /// Provider used by the job's delegated agent.
    pub provider: String,
    /// What the job is doing right now (last tool-call summary), for live UI.
    pub activity: Option<String>,
    /// Recent tool-call summaries (up to [`MAX_TRACE_LINES`]), appended to
    /// error results so a failed job is diagnosable.
    pub trace: Vec<String>,
    /// Bounded snapshot of the agent's runtime events for in-progress inspection.
    #[serde(skip)]
    pub events: Vec<JobEvent>,
    /// Full conversation transcript, kept on successful completion so
    /// `ctx.agent.followup` can resume this agent with its context intact.
    #[serde(skip)]
    pub transcript: Option<Vec<crate::llm::ChatMessage>>,
    /// Runtime background scope the job belongs to. Durable conversations use
    /// their database id; incognito sessions use an actor-unique identity.
    /// `None` is reserved for standalone callers that have no runtime owner.
    #[serde(skip)]
    pub scope: Option<i64>,
    /// Per-job cancellation flag, settable by [`JobRegistry::cancel`].
    #[serde(skip)]
    pub cancel_flag: Arc<AtomicBool>,
}

impl Job {
    /// Done or Error — the job will never make further progress.
    pub fn is_finished(&self) -> bool {
        matches!(self.status, JobStatus::Done | JobStatus::Error)
    }
}

/// Parameters for [`JobRegistry::create`]. Bundled into a struct so the call
/// sites stay readable and new fields (e.g. Tier 3 scope) don't grow the
/// positional argument list.
pub struct NewJob {
    /// Agent template name used for display.
    pub agent: String,
    /// The task prompt handed to the agent.
    pub task: String,
    /// Short human-readable summary for display (empty falls back to `task`).
    pub title: String,
    /// Provider used by this delegated run.
    pub provider: String,
    /// Runtime owner used to scope queries, cancellation, and result injection.
    pub scope: Option<i64>,
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

    /// Lock the jobs mutex, panicking on poison (same as `unwrap_or_else`).
    fn lock_jobs(&self) -> std::sync::MutexGuard<'_, Vec<Job>> {
        self.jobs.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Create a queued job. Its runner marks it Running after acquiring a
    /// cross-process provider slot.
    pub fn create(&self, job: NewJob) -> String {
        let NewJob {
            agent,
            task,
            title,
            provider,
            scope,
            cancel_flag,
        } = job;
        let now = current_unix_seconds();
        let mut jobs = self.lock_jobs();
        let id = format!("job-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        jobs.push(Job {
            id: id.clone(),
            agent,
            task,
            title,
            status: JobStatus::Queued,
            result: None,
            started_at: now,
            finished_at: None,
            consumed: false,
            token_sent: 0,
            token_received: 0,
            result_file: None,
            provider,
            activity: None,
            trace: Vec::new(),
            events: Vec::new(),
            transcript: None,
            scope,
            cancel_flag,
        });
        // Prune oldest finished-and-consumed jobs to cap memory growth.
        if jobs.len() > MAX_RETAINED_JOBS {
            let excess = jobs.len() - MAX_RETAINED_JOBS;
            let mut removed = 0;
            jobs.retain(|j| {
                if removed < excess && j.is_finished() && j.consumed {
                    removed += 1;
                    false
                } else {
                    true
                }
            });
        }
        drop(jobs);
        self.version.fetch_add(1, Ordering::Relaxed);
        id
    }

    /// Mark a queued job Running after its provider slot has been acquired.
    pub fn start(&self, id: &str) -> bool {
        let now = current_unix_seconds();
        let mut jobs = self.lock_jobs();
        let Some(job) = jobs.iter_mut().find(|job| job.id == id) else {
            return false;
        };
        if job.status != JobStatus::Queued {
            return job.status == JobStatus::Running;
        }
        job.status = JobStatus::Running;
        job.started_at = now;
        drop(jobs);
        self.version.fetch_add(1, Ordering::Relaxed);
        true
    }

    /// Record what a running job is doing (tool-call summary): sets the live
    /// activity label and appends to the bounded trace.
    pub fn note_activity(&self, id: &str, summary: &str) {
        let mut jobs = self.lock_jobs();
        if let Some(job) = jobs.iter_mut().find(|j| j.id == id) {
            job.activity = Some(summary.to_string());
            if job.trace.len() >= MAX_TRACE_LINES {
                job.trace.remove(0);
            }
            job.trace.push(summary.to_string());
            drop(jobs);
            self.version.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Clear the live activity label when a tool call finishes; a failed call
    /// marks its trace entry so error reports show where things went wrong.
    pub fn note_activity_done(&self, id: &str, is_error: bool) {
        let mut jobs = self.lock_jobs();
        if let Some(job) = jobs.iter_mut().find(|j| j.id == id) {
            job.activity = None;
            if is_error && let Some(last) = job.trace.last_mut() {
                last.push_str(" ✗");
            }
            drop(jobs);
            self.version.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Retain a bounded event stream so the TUI can inspect a running agent.
    pub fn note_event(
        &self,
        id: &str,
        event: crate::runtime::RuntimeEvent,
        edit_preview: Option<String>,
    ) {
        const MAX_EVENTS: usize = 500;
        let mut jobs = self.lock_jobs();
        if let Some(job) = jobs.iter_mut().find(|j| j.id == id) {
            if job.events.len() >= MAX_EVENTS {
                job.events.remove(0);
            }
            job.events.push(JobEvent {
                event,
                edit_preview,
            });
            drop(jobs);
            self.version.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Recent tool-call summaries for a job (for error reports).
    pub fn trace_of(&self, id: &str) -> Vec<String> {
        let jobs = self.lock_jobs();
        jobs.iter()
            .find(|j| j.id == id)
            .map(|j| j.trace.clone())
            .unwrap_or_default()
    }

    /// The saved transcript of a finished job, or `None` when the job is
    /// unknown, still running, or completed without one (errors).
    pub fn transcript_of(
        &self,
        id: &str,
        scope: Option<i64>,
    ) -> Option<Vec<crate::llm::ChatMessage>> {
        let jobs = self.lock_jobs();
        let job = jobs
            .iter()
            .find(|j| j.id == id && scope_matches(j.scope, Some(scope)))?;
        job.transcript.clone()
    }

    /// Mark a job as finished (Ok or Error).
    pub fn complete(&self, id: &str, result: Result<String, String>) {
        self.complete_with_tokens(id, result, 0, 0, None);
    }

    /// Update token counts for a running job.
    pub fn update_tokens(&self, id: &str, sent: u64, received: u64) {
        let mut jobs = self.lock_jobs();
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
    /// `transcript`, when `Some`, is retained for `ctx.agent.followup`.
    pub fn complete_with_tokens(
        &self,
        id: &str,
        result: Result<String, String>,
        token_sent: u64,
        token_received: u64,
        transcript: Option<Vec<crate::llm::ChatMessage>>,
    ) {
        let now = current_unix_seconds();
        let status = if result.is_ok() {
            JobStatus::Done
        } else {
            JobStatus::Error
        };
        let result = result.unwrap_or_else(|e| e);
        let result_file = spill_result(id, &result);
        let mut jobs = self.lock_jobs();
        finish_job(
            &mut jobs,
            id,
            result,
            result_file,
            status,
            now,
            token_sent,
            token_received,
            transcript,
        );
        self.completed.notify_all();
        drop(jobs);
        self.version.fetch_add(1, Ordering::Relaxed);
    }

    /// IDs of all active (running or queued) jobs.
    pub fn running_ids(&self) -> Vec<String> {
        let jobs = self.lock_jobs();
        jobs.iter()
            .filter(|j| !j.is_finished() && scope_matches(j.scope, None))
            .map(|j| j.id.clone())
            .collect()
    }

    /// IDs of active jobs owned by exactly `scope`.
    pub fn running_ids_scoped(&self, scope: Option<i64>) -> Vec<String> {
        let jobs = self.lock_jobs();
        jobs.iter()
            .filter(|j| !j.is_finished() && scope_matches(j.scope, Some(scope)))
            .map(|j| j.id.clone())
            .collect()
    }

    /// Clones of all active (running or queued) jobs.
    pub fn running_jobs(&self) -> Vec<Job> {
        let jobs = self.lock_jobs();
        jobs.iter()
            .filter(|j| !j.is_finished() && scope_matches(j.scope, None))
            .cloned()
            .collect()
    }

    /// Clones of active jobs in `scope` (see [`Job::scope`]).
    pub fn running_jobs_scoped(&self, scope: Option<i64>) -> Vec<Job> {
        let jobs = self.lock_jobs();
        jobs.iter()
            .filter(|j| !j.is_finished() && scope_matches(j.scope, Some(scope)))
            .cloned()
            .collect()
    }

    /// Block until all of the given jobs finish, the timeout elapses, or the
    /// cancellation flag is set. Finished jobs (among `ids`) are returned and
    /// marked consumed so they are not auto-injected again later. IDs unknown
    /// to the registry are ignored. Jobs still running when the wait ends are
    /// reported as `pending`. Ordinary pending jobs remain unconsumed and
    /// auto-inject on completion; jobs explicitly cancelled through the
    /// registry remain consumed.
    pub fn wait_for(
        &self,
        ids: &[String],
        timeout: Duration,
        cancelled: Option<&AtomicBool>,
    ) -> WaitOutcome {
        self.wait_for_matching(ids, timeout, cancelled, None)
    }

    /// Scoped form of [`wait_for`](Self::wait_for). Unknown or foreign job ids
    /// are ignored and can never be consumed by another actor.
    pub fn wait_for_scoped(
        &self,
        ids: &[String],
        timeout: Duration,
        cancelled: Option<&AtomicBool>,
        scope: Option<i64>,
    ) -> WaitOutcome {
        self.wait_for_matching(ids, timeout, cancelled, Some(scope))
    }

    fn wait_for_matching(
        &self,
        ids: &[String],
        timeout: Duration,
        cancelled: Option<&AtomicBool>,
        required_scope: Option<Option<i64>>,
    ) -> WaitOutcome {
        let deadline = Instant::now() + timeout;
        let mut jobs = self.lock_jobs();
        loop {
            let pending: Vec<String> = jobs
                .iter()
                .filter(|j| {
                    !j.is_finished()
                        && ids.contains(&j.id)
                        && scope_matches(j.scope, required_scope)
                })
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
                    .filter(|j| {
                        j.is_finished()
                            && ids.contains(&j.id)
                            && scope_matches(j.scope, required_scope)
                    })
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
                .unwrap_or_else(|e| e.into_inner());
            jobs = guard;
        }
    }

    /// Snapshot of all jobs as a JSON array.
    pub fn snapshot(&self) -> serde_json::Value {
        let jobs = self.lock_jobs();
        let array: Vec<_> = jobs
            .iter()
            .filter(|job| scope_matches(job.scope, None))
            .cloned()
            .collect();
        serde_json::to_value(array).unwrap_or_else(|_| serde_json::json!([]))
    }

    /// Snapshot of jobs owned by exactly `scope`.
    pub fn snapshot_scoped(&self, scope: Option<i64>) -> serde_json::Value {
        let jobs = self.lock_jobs();
        let array: Vec<_> = jobs
            .iter()
            .filter(|job| scope_matches(job.scope, Some(scope)))
            .cloned()
            .collect();
        serde_json::to_value(array).unwrap_or_else(|_| serde_json::json!([]))
    }

    /// Clones of all jobs (e.g. for the Rust-side pane renderer).
    pub fn all_jobs(&self) -> Vec<Job> {
        self.lock_jobs().clone()
    }

    /// Peek at all unconsumed finished jobs without marking them consumed.
    /// Call [`JobRegistry::mark_consumed`] after the results have actually
    /// been delivered (e.g. injected into the conversation).
    pub fn peek_finished_unconsumed(&self) -> Vec<Job> {
        let jobs = self.lock_jobs();
        let mut finished: Vec<Job> = jobs
            .iter()
            .filter(|j| {
                (j.status == JobStatus::Done || j.status == JobStatus::Error)
                    && !j.consumed
                    && scope_matches(j.scope, None)
            })
            .cloned()
            .collect();
        finished.sort_by_key(|j| j.started_at);
        finished
    }

    /// Like [`peek_finished_unconsumed`](Self::peek_finished_unconsumed) but
    /// limited to jobs in `scope` (see [`Job::scope`]), so a daemon only injects
    /// results from its own conversation.
    pub fn peek_finished_unconsumed_scoped(&self, scope: Option<i64>) -> Vec<Job> {
        let jobs = self.lock_jobs();
        let mut finished: Vec<Job> = jobs
            .iter()
            .filter(|j| {
                scope_matches(j.scope, Some(scope))
                    && (j.status == JobStatus::Done || j.status == JobStatus::Error)
                    && !j.consumed
            })
            .cloned()
            .collect();
        finished.sort_by_key(|j| j.started_at);
        finished
    }

    /// Cancel an unfinished job by setting its cancel flag. Explicitly
    /// cancelled jobs are also marked consumed so their eventual cancellation
    /// result is not auto-injected into a later turn. Returns `true` when the
    /// job was found and signalled.
    pub fn cancel(&self, id: &str) -> bool {
        self.cancel_matching(id, None)
    }

    /// Cancel an unfinished job only when it belongs to `scope`.
    ///
    /// This is the entry point for conversation-owned APIs. The id and scope
    /// check happen under the same registry lock so another conversation can
    /// never cancel or consume the job between a snapshot and the mutation.
    pub fn cancel_scoped(&self, id: &str, scope: Option<i64>) -> bool {
        self.cancel_matching(id, Some(scope))
    }

    fn cancel_matching(&self, id: &str, required_scope: Option<Option<i64>>) -> bool {
        let mut jobs = self.lock_jobs();
        let Some(job) = jobs
            .iter_mut()
            .find(|job| job.id == id && scope_matches(job.scope, required_scope))
        else {
            return false;
        };
        if job.is_finished() {
            return false;
        }
        job.cancel_flag.store(true, Ordering::Relaxed);
        job.activity = Some("cancelling…".to_string());
        job.consumed = true;
        self.completed.notify_all();
        drop(jobs);
        self.version.fetch_add(1, Ordering::Relaxed);
        true
    }

    /// Cancel every running job in `scope` (see [`Job::scope`]). Returns the
    /// number signalled. Used by a daemon's conversation-reset / turn-cancel so
    /// it only stops its own conversation's sub-agents, not those of another
    /// conversation sharing the process (`bone serve`).
    pub fn cancel_all_scoped(&self, scope: Option<i64>) -> usize {
        let mut jobs = self.lock_jobs();
        let mut cancelled = 0;
        for job in jobs.iter_mut() {
            if !job.is_finished() && scope_matches(job.scope, Some(scope)) {
                job.cancel_flag.store(true, Ordering::Relaxed);
                job.activity = Some("cancelling…".to_string());
                job.consumed = true;
                cancelled += 1;
            }
        }
        if cancelled > 0 {
            self.completed.notify_all();
            self.version.fetch_add(1, Ordering::Relaxed);
        }
        cancelled
    }

    /// Mark the given job ids as consumed. Bumps the version if any changed.
    pub fn mark_consumed(&self, ids: &[String]) {
        let mut jobs = self.lock_jobs();
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

/// Return whether a job's scope matches an optional scope restriction.
/// `None` means unscoped callers may see every job; `Some(scope)` requires an
/// exact match, including `Some(None)` for jobs with no scope.
fn scope_matches(job_scope: Option<i64>, required_scope: Option<Option<i64>>) -> bool {
    required_scope.is_none_or(|scope| job_scope == scope)
}

/// Apply finished-job state under the lock: update status/result/tokens.
#[allow(clippy::too_many_arguments)]
fn finish_job(
    jobs: &mut [Job],
    id: &str,
    result: String,
    result_file: Option<String>,
    status: JobStatus,
    now: u64,
    token_sent: u64,
    token_received: u64,
    transcript: Option<Vec<crate::llm::ChatMessage>>,
) {
    if let Some(job) = jobs.iter_mut().find(|j| j.id == id) {
        job.status = status;
        job.result = Some(result);
        job.finished_at = Some(now);
        job.result_file = result_file;
        job.token_sent = token_sent;
        job.token_received = token_received;
        job.activity = None;
        job.transcript = transcript;
    }
}

pub fn current_unix_seconds() -> u64 {
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

/// Status glyph for a subagent job (done / error / running / queued).
pub fn status_sym(status: JobStatus) -> &'static str {
    match status {
        JobStatus::Done => "✓",
        JobStatus::Error => "✗",
        JobStatus::Running => "◑",
        JobStatus::Queued => "⧗",
    }
}

/// Build the injected turn for a batch of finished background jobs: the full
/// prompt handed to the model (`turn_text`) and a short label for the frontend
/// scrollback (`display_text`). `None` when `finished` is empty.
///
/// Used by daemon-owned background injection. `still_running` is appended as a
/// note so the model doesn't assume outstanding jobs failed.
pub fn format_results_for_injection(
    finished: &[Job],
    still_running: &[Job],
) -> Option<(String, String)> {
    if finished.is_empty() {
        return None;
    }
    let mut lines = Vec::with_capacity(finished.len());
    for job in finished {
        let mut truncated =
            truncate_for_injection(job.result.as_deref().unwrap_or(""), MAX_INJECT_CHARS);
        if let Some(file) = &job.result_file {
            truncated.push_str(&format!("\n[full output saved to: {file}]"));
        }
        lines.push(format!(
            "## {} ({}) — {}\n{}",
            job.agent,
            job.id,
            status_sym(job.status),
            truncated
        ));
    }
    if !still_running.is_empty() {
        let names: Vec<String> = still_running
            .iter()
            .map(|j| format!("{} ({})", j.agent, j.id))
            .collect();
        lines.push(format!(
            "Note: still running: {}. Their results will arrive automatically in a later message — do not assume their outcome.",
            names.join(", ")
        ));
    }
    let turn_text = format!(
        "[automated message] Results from background jobs you dispatched earlier are now ready. \
         Review them and continue the task they were dispatched for; if nothing remains to be done, \
         summarize the outcomes for the user.\n\n{}",
        lines.join("\n\n")
    );
    let display: String = finished
        .iter()
        .map(|j| format!("{} {}", j.agent, status_sym(j.status)))
        .collect::<Vec<_>>()
        .join(", ");
    let display_text = format!("[job results: {display}]");
    Some((turn_text, display_text))
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
    // Prefer a nearby whitespace boundary, but do not discard most of the
    // budget just because the previous word is unusually long.
    let boundary_window = max_chars.div_ceil(10).max(1);
    let kept = truncated
        .char_indices()
        .rev()
        .take(boundary_window)
        .find(|(idx, ch)| *idx > 0 && ch.is_whitespace())
        .map_or(truncated, |(idx, _)| &truncated[..idx]);
    format!("{kept}{TRUNCATION_MARKER}")
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "jobs_tests.rs"]
mod jobs_tests;

//! Host-managed shell processes shared by native tools and Lua extensions.
//!
//! These are deliberately not raw child handles exposed to plugins: Bone owns
//! cancellation, bounded captured output, and the process-group lifetime.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use std::time::{SystemTime, UNIX_EPOCH};

use crate::tools::shell::{ScriptRequest, run_script_stream_with_metadata};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessState {
    Running,
    Exited,
    TimedOut,
    Cancelled,
}

impl ProcessState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Exited => "exited",
            Self::TimedOut => "timed out",
            Self::Cancelled => "cancelled",
        }
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

#[derive(Clone, Debug)]
pub struct ProcessSnapshot {
    pub id: String,
    pub command: String,
    pub owner: String,
    pub running: bool,
    pub state: ProcessState,
    pub started_at: u64,
    pub finished_at: Option<u64>,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub error: Option<String>,
}

struct Process {
    snapshot: ProcessSnapshot,
    cancel: Arc<AtomicBool>,
    order: u64,
}

const MAX_COMPLETED_PROCESSES: usize = 64;
const LIVE_OUTPUT_BYTES: usize = 64 * 1024;
const LIVE_OUTPUT_MARKER: &str = "... earlier live output truncated ...\n";

pub struct ProcessRegistry {
    next: AtomicU64,
    version: AtomicU64,
    processes: Mutex<HashMap<String, Process>>,
}

fn append_bounded(output: &mut String, bytes: &[u8]) {
    output.push_str(&String::from_utf8_lossy(bytes));
    if output.len() <= LIVE_OUTPUT_BYTES {
        return;
    }
    let keep = LIVE_OUTPUT_BYTES - LIVE_OUTPUT_MARKER.len();
    let mut start = output.len() - keep;
    while !output.is_char_boundary(start) {
        start += 1;
    }
    let tail = output[start..].to_owned();
    output.clear();
    output.push_str(LIVE_OUTPUT_MARKER);
    output.push_str(&tail);
}

impl ProcessRegistry {
    pub fn spawn(
        &self,
        command: String,
        owner: String,
        timeout_ms: u64,
        working_dir: Option<std::path::PathBuf>,
    ) -> String {
        let order = self.next.fetch_add(1, Ordering::Relaxed);
        let id = format!("process-{order}");
        let cancel = Arc::new(AtomicBool::new(false));
        let started_at = now_millis();
        self.processes.lock().unwrap().insert(
            id.clone(),
            Process {
                snapshot: ProcessSnapshot {
                    id: id.clone(),
                    command: command.clone(),
                    owner,
                    running: true,
                    state: ProcessState::Running,
                    started_at,
                    finished_at: None,
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: None,
                    signal: None,
                    error: None,
                },
                cancel: cancel.clone(),
                order,
            },
        );
        self.bump_version();
        let registry = registry();
        let job_id = id.clone();
        tokio::spawn(async move {
            let output_id = job_id.clone();
            let result = run_script_stream_with_metadata(
                ScriptRequest {
                    command,
                    env: Vec::new(),
                    timeout_ms,
                    working_dir,
                    cancel: Some(cancel),
                },
                |is_stderr, bytes| {
                    registry.append_output(&output_id, is_stderr, bytes);
                    Ok(())
                },
            )
            .await;
            let mut processes = registry.processes.lock().unwrap();
            if let Some(process) = processes.get_mut(&job_id) {
                process.snapshot.running = false;
                process.snapshot.finished_at = Some(now_millis());
                match result {
                    Ok(out) => {
                        process.snapshot.state = if out.cancelled {
                            ProcessState::Cancelled
                        } else if out.timed_out {
                            ProcessState::TimedOut
                        } else {
                            ProcessState::Exited
                        };
                        process.snapshot.stdout.clear();
                        append_bounded(&mut process.snapshot.stdout, out.stdout.as_bytes());
                        process.snapshot.stderr.clear();
                        append_bounded(&mut process.snapshot.stderr, out.stderr.as_bytes());
                        process.snapshot.exit_code = out.exit_code;
                        process.snapshot.signal = out.signal;
                    }
                    Err(err) => {
                        process.snapshot.state = ProcessState::Exited;
                        process.snapshot.error = Some(err);
                    }
                }
            }
            Self::prune_completed(&mut processes);
            drop(processes);
            registry.bump_version();
        });
        id
    }

    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Relaxed)
    }

    fn bump_version(&self) {
        self.version.fetch_add(1, Ordering::Relaxed);
    }

    fn append_output(&self, id: &str, is_stderr: bool, bytes: &[u8]) {
        let mut processes = self.processes.lock().unwrap();
        let Some(process) = processes.get_mut(id) else {
            return;
        };
        let output = if is_stderr {
            &mut process.snapshot.stderr
        } else {
            &mut process.snapshot.stdout
        };
        append_bounded(output, bytes);
        drop(processes);
        self.bump_version();
    }

    fn prune_completed(processes: &mut HashMap<String, Process>) {
        while processes.values().filter(|p| !p.snapshot.running).count() > MAX_COMPLETED_PROCESSES {
            let Some(id) = processes
                .values()
                .filter(|p| !p.snapshot.running)
                .min_by_key(|p| p.order)
                .map(|p| p.snapshot.id.clone())
            else {
                break;
            };
            processes.remove(&id);
        }
    }

    pub fn get(&self, id: &str) -> Option<ProcessSnapshot> {
        self.processes
            .lock()
            .unwrap()
            .get(id)
            .map(|p| p.snapshot.clone())
    }
    pub fn get_scoped(&self, scope: &str, id: &str) -> Option<ProcessSnapshot> {
        self.get(id).filter(|process| process.owner == scope)
    }

    pub fn list(&self, owner: Option<&str>) -> Vec<ProcessSnapshot> {
        let mut processes: Vec<_> = self
            .processes
            .lock()
            .unwrap()
            .values()
            .filter(|p| owner.is_none_or(|o| p.snapshot.owner == o))
            .map(|p| (p.order, p.snapshot.clone()))
            .collect();
        processes.sort_unstable_by_key(|(order, _)| std::cmp::Reverse(*order));
        processes
            .into_iter()
            .map(|(_, snapshot)| snapshot)
            .collect()
    }
    pub fn kill_scoped(&self, scope: &str, id: &str) -> bool {
        let processes = self.processes.lock().unwrap();
        let Some(p) = processes
            .get(id)
            .filter(|process| process.snapshot.owner == scope)
        else {
            return false;
        };
        if !p.snapshot.running {
            return false;
        }
        p.cancel.store(true, Ordering::Relaxed);
        drop(processes);
        self.bump_version();
        true
    }

    /// Request cancellation for every running process owned by `scope`.
    pub fn kill_all_scoped(&self, scope: &str) -> usize {
        let processes = self.processes.lock().unwrap();
        let mut killed = 0;
        for process in processes.values() {
            if process.snapshot.owner == scope && process.snapshot.running {
                process.cancel.store(true, Ordering::Relaxed);
                killed += 1;
            }
        }
        drop(processes);
        if killed > 0 {
            self.bump_version();
        }
        killed
    }

    /// Remove every finished process owned by `scope`, returning the number
    /// removed. Called when a new user turn starts: a finished process stays
    /// visible for the turn in which it finished, then the next turn clears it.
    pub fn clear_completed_scoped(&self, scope: &str) -> usize {
        let mut removed = 0;
        let mut processes = self.processes.lock().unwrap();
        processes.retain(|_, process| {
            let finished = process.snapshot.owner == scope && !process.snapshot.running;
            if finished {
                removed += 1;
            }
            !finished
        });
        drop(processes);
        if removed > 0 {
            self.bump_version();
        }
        removed
    }

    pub fn kill(&self, id: &str) -> bool {
        let processes = self.processes.lock().unwrap();
        let Some(p) = processes.get(id) else {
            return false;
        };
        if !p.snapshot.running {
            return false;
        }
        p.cancel.store(true, Ordering::Relaxed);
        drop(processes);
        self.bump_version();
        true
    }
}
pub fn conversation_scope(session_id: Option<i64>) -> String {
    session_id.map_or_else(
        || "conversation:local".into(),
        |id| format!("conversation:{id}"),
    )
}

pub fn registry() -> &'static ProcessRegistry {
    static REG: OnceLock<ProcessRegistry> = OnceLock::new();
    REG.get_or_init(|| ProcessRegistry {
        next: AtomicU64::new(1),
        version: AtomicU64::new(1),
        processes: Mutex::new(HashMap::new()),
    })
}

fn elapsed_ms(process: &ProcessSnapshot) -> u64 {
    process
        .finished_at
        .unwrap_or_else(now_millis)
        .saturating_sub(process.started_at)
}

fn format_elapsed(process: &ProcessSnapshot) -> String {
    let seconds = elapsed_ms(process) / 1000;
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        format!("{}m{}s", seconds / 60, seconds % 60)
    }
}

pub(crate) fn execute_action(
    action: &str,
    id: Option<&str>,
    scope: Option<&str>,
) -> Result<String, String> {
    match action {
        "list" => Ok(registry()
            .list(scope)
            .into_iter()
            .map(|process| {
                format!(
                    "{} {} {} ({})",
                    process.id,
                    process.state.label(),
                    format_elapsed(&process),
                    process.command
                )
            })
            .collect::<Vec<_>>()
            .join("\n")),
        "status" => {
            let id = id.ok_or("id is required for status")?;
            let process = match scope {
                Some(scope) => registry().get_scoped(scope, id),
                None => registry().get(id),
            }
            .ok_or("unknown process")?;
            Ok(format!(
                "{}\nstate: {}\nelapsed: {}\nrunning: {}\nexit code: {}\nsignal: {}\nstdout:\n{}\nstderr:\n{}\n{}",
                process.id,
                process.state.label(),
                format_elapsed(&process),
                process.running,
                process
                    .exit_code
                    .map_or_else(|| "none".into(), |code| code.to_string()),
                process
                    .signal
                    .map_or_else(|| "none".into(), |signal| signal.to_string()),
                process.stdout,
                process.stderr,
                process.error.unwrap_or_default()
            ))
        }
        "kill" => {
            let id = id.ok_or("id is required for kill")?;
            let killed = match scope {
                Some(scope) => registry().kill_scoped(scope, id),
                None => registry().kill(id),
            };
            if killed {
                Ok(format!("stop requested for {id}"))
            } else {
                Err(format!("process {id} is unknown or already finished"))
            }
        }
        _ => Err("action must be run, list, status, or kill".into()),
    }
}

#[cfg(test)]
#[path = "processes_tests.rs"]
mod tests;

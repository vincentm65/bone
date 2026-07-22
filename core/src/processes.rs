//! Host-managed shell processes shared by native tools and Lua extensions.
//!
//! These are deliberately not raw child handles exposed to plugins: Bone owns
//! cancellation, bounded captured output, and the process-group lifetime.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::tools::shell::{ScriptRequest, run_script};

#[derive(Clone, Debug)]
pub struct ProcessSnapshot {
    pub id: String,
    pub command: String,
    pub owner: String,
    pub running: bool,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
}

struct Process {
    snapshot: ProcessSnapshot,
    cancel: Arc<AtomicBool>,
}
pub struct ProcessRegistry {
    next: AtomicU64,
    processes: Mutex<HashMap<String, Process>>,
}

impl ProcessRegistry {
    pub fn spawn(
        &self,
        command: String,
        owner: String,
        timeout_ms: u64,
        working_dir: Option<std::path::PathBuf>,
    ) -> String {
        let id = format!("process-{}", self.next.fetch_add(1, Ordering::Relaxed));
        let cancel = Arc::new(AtomicBool::new(false));
        self.processes.lock().unwrap().insert(
            id.clone(),
            Process {
                snapshot: ProcessSnapshot {
                    id: id.clone(),
                    command: command.clone(),
                    owner,
                    running: true,
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: None,
                    error: None,
                },
                cancel: cancel.clone(),
            },
        );
        let registry = registry();
        let job_id = id.clone();
        tokio::spawn(async move {
            let result = run_script(ScriptRequest {
                command,
                env: Vec::new(),
                timeout_ms,
                working_dir,
                cancel: Some(cancel),
            })
            .await;
            if let Some(process) = registry.processes.lock().unwrap().get_mut(&job_id) {
                process.snapshot.running = false;
                match result {
                    Ok(out) => {
                        process.snapshot.stdout = out.stdout;
                        process.snapshot.stderr = out.stderr;
                        process.snapshot.exit_code = out.exit_code;
                    }
                    Err(err) => process.snapshot.error = Some(err),
                }
            }
        });
        id
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
        self.processes
            .lock()
            .unwrap()
            .values()
            .filter(|p| owner.is_none_or(|o| p.snapshot.owner == o))
            .map(|p| p.snapshot.clone())
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
        killed
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
        processes: Mutex::new(HashMap::new()),
    })
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
                    "{} {} {}",
                    process.id,
                    if process.running {
                        "running"
                    } else {
                        "finished"
                    },
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
                "{}\nrunning: {}\nstdout:\n{}\nstderr:\n{}\n{}",
                process.id,
                process.running,
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
mod tests {
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
                error: None,
            },
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    #[test]
    fn kill_all_scoped_only_cancels_running_processes_in_scope() {
        let registry = ProcessRegistry {
            next: AtomicU64::new(1),
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
}

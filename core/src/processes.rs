//! Host-managed shell processes shared by native tools and Lua extensions.
//!
//! These are deliberately not raw child handles exposed to plugins: Bone owns
//! cancellation, bounded captured output, and the process-group lifetime.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::tools::shell::{ScriptRequest, run_script};
use crate::tools::{Tool, ToolDefinition};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

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
    pub fn spawn(&self, command: String, owner: String, timeout_ms: u64) -> String {
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
    pub fn list(&self, owner: Option<&str>) -> Vec<ProcessSnapshot> {
        self.processes
            .lock()
            .unwrap()
            .values()
            .filter(|p| owner.is_none_or(|o| p.snapshot.owner == o))
            .map(|p| p.snapshot.clone())
            .collect()
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
pub fn registry() -> &'static ProcessRegistry {
    static REG: OnceLock<ProcessRegistry> = OnceLock::new();
    REG.get_or_init(|| ProcessRegistry {
        next: AtomicU64::new(1),
        processes: Mutex::new(HashMap::new()),
    })
}

/// Agent-facing controls for managed background shell jobs.  This stays
/// separate from `shell` so the model never needs to issue an OS-level kill
/// command or learn an implementation PID.
pub struct ProcessTool;
#[derive(Deserialize)]
struct ProcessArgs {
    action: String,
    #[serde(default)]
    id: Option<String>,
}
#[async_trait]
impl Tool for ProcessTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "process".into(),
            description: "Inspect or stop managed background shell processes.".into(),
            input_schema: json!({"type":"object","properties":{"action":{"type":"string","enum":["list","status","kill"]},"id":{"type":"string"}},"required":["action"],"additionalProperties":false}),
        }
    }
    async fn execute(&self, arguments: Value) -> Result<String, String> {
        let args: ProcessArgs = serde_json::from_value(arguments).map_err(crate::util::errstr)?;
        match args.action.as_str() {
            "list" => Ok(registry()
                .list(None)
                .into_iter()
                .map(|p| {
                    format!(
                        "{} {} {}",
                        p.id,
                        if p.running { "running" } else { "finished" },
                        p.command
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")),
            "status" => {
                let id = args.id.ok_or("id is required for status")?;
                let p = registry().get(&id).ok_or("unknown process")?;
                Ok(format!(
                    "{}\nrunning: {}\nstdout:\n{}\nstderr:\n{}\n{}",
                    p.id,
                    p.running,
                    p.stdout,
                    p.stderr,
                    p.error.unwrap_or_default()
                ))
            }
            "kill" => {
                let id = args.id.ok_or("id is required for kill")?;
                if registry().kill(&id) {
                    Ok(format!("stop requested for {id}"))
                } else {
                    Err(format!("process {id} is unknown or already finished"))
                }
            }
            _ => Err("action must be list, status, or kill".into()),
        }
    }
}

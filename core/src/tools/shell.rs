//! The `shell` / `bash` tool: runs commands with streaming output and timeouts.

use std::process::Stdio;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::task::JoinHandle;
use tokio::time::{Duration, timeout};

use crate::tools::truncate_line;
use crate::tools::types::{Tool, ToolDefinition, ToolExecutionContext, ToolOutput};

// ── Script execution (formerly script_runner.rs) ────────────────────────────

#[cfg(unix)]
unsafe extern "C" {
    fn setsid() -> i32;
    fn kill(pid: i32, sig: i32) -> i32;
}

#[cfg(unix)]
const SIGKILL: i32 = 9;

/// Signal the child's whole process group so grandchildren (the actual
/// download/build the shell spawned) die with it, not just the wrapper.
/// `setsid()` in `pre_exec` makes the child a group leader (pgid == pid), so a
/// signal to the group (`-pid`) reaches the entire tree.
#[cfg(unix)]
fn kill_process_group(pid: u32) {
    // Negative pid => the whole process group.
    unsafe {
        let _ = kill(-(pid as i32), SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_process_group(_pid: u32) {}

pub struct ScriptRequest {
    pub command: String,
    pub env: Vec<(String, String)>,
    pub timeout_ms: u64,
    /// Cooperative cancel flag. When set (Esc/Ctrl+C mid-turn), `run_script`
    /// kills the process tree and returns promptly with partial output instead
    /// of blocking until `timeout_ms`. `None` for the context-less paths
    /// (`ctx.shell`), where the wall-clock timeout is the only backstop.
    pub cancel: Option<Arc<AtomicBool>>,
}

pub struct ScriptOutput {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Returns the shell program, its argument flag, and a label for descriptions.
pub fn shell_command() -> (&'static str, &'static str, &'static str) {
    static SHELL: OnceLock<(&'static str, &'static str, &'static str)> = OnceLock::new();
    *SHELL.get_or_init(detect_shell_command)
}

fn detect_shell_command() -> (&'static str, &'static str, &'static str) {
    if cfg!(windows) {
        if which("pwsh") {
            ("pwsh", "-Command", "pwsh -Command")
        } else {
            ("powershell", "-Command", "powershell -Command")
        }
    } else {
        ("bash", "-lc", "bash -lc")
    }
}

fn which(name: &str) -> bool {
    std::process::Command::new(name)
        .arg("-Version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

pub async fn run_script(request: ScriptRequest) -> Result<ScriptOutput, String> {
    if request.command.contains('\0') {
        return Err("shell command must not contain NUL bytes".to_string());
    }
    let timeout_ms = request.timeout_ms.clamp(1_000, 3_600_000);
    let cancel = request.cancel.clone();
    let (shell, shell_arg, _) = shell_command();
    let mut cmd = Command::new(shell);
    cmd.arg(shell_arg)
        .arg(&request.command)
        .envs(request.env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Detach controlling tty so sudo/ssh (reading /dev/tty) fail cleanly
    // instead of corrupting the TUI and swallowing keystrokes.
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| {
            setsid();
            Ok(())
        });
    }
    let mut child = cmd.spawn().map_err(crate::util::errstr)?;

    let mut stdout = child.stdout.take().ok_or("failed to capture stdout")?;
    let mut stderr = child.stderr.take().ok_or("failed to capture stderr")?;

    // Read stdout/stderr in dedicated tasks so partial output survives a
    // timeout. With an inline read_to_end inside the wait future, data already
    // pulled from the pipe is lost when the future is cancelled — the
    // post-timeout drain would see an empty buffer.
    let stdout_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf).await;
        buf
    });
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf).await;
        buf
    });

    // Race the child against both the wall-clock timeout and a cooperative
    // cancel (Esc). Whichever fires first wins; `select!` drops the losing
    // branch future, which releases the `&mut child` borrow held by the
    // `child.wait()` future so the kill/reap below can run.
    let outcome = tokio::select! {
        biased;
        _ = await_cancel(cancel.as_ref()) => WaitOutcome::Cancelled,
        r = timeout(Duration::from_millis(timeout_ms), child.wait()) => match r {
            Ok(Ok(status)) => WaitOutcome::Exited(status),
            Ok(Err(e)) => return Err(crate::util::errstr(e)),
            Err(_) => WaitOutcome::TimedOut,
        },
    };

    match outcome {
        WaitOutcome::Exited(status) => {
            let out = stdout_task.await.unwrap_or_default();
            let err = stderr_task.await.unwrap_or_default();
            Ok(ScriptOutput {
                exit_code: status.code(),
                signal: exit_signal(&status),
                stdout: truncate_output(&String::from_utf8_lossy(&out), 500),
                stderr: truncate_output(&String::from_utf8_lossy(&err), 100),
            })
        }
        WaitOutcome::TimedOut => {
            let (stdout_str, stderr_str) =
                kill_and_drain(&mut child, stdout_task, stderr_task).await;
            let mut msg =
                format!("[timed out after {timeout_ms}ms; partial output]\nstdout:\n{stdout_str}");
            if !stderr_str.is_empty() {
                msg.push_str(&format!("\nstderr:\n{stderr_str}"));
            }
            Err(msg)
        }
        WaitOutcome::Cancelled => {
            // Esc/Ctrl+C mid-command: kill the whole tree and return whatever
            // output was captured, so a stuck download no longer freezes the
            // machine until the wall-clock timeout.
            let (stdout_str, stderr_str) =
                kill_and_drain(&mut child, stdout_task, stderr_task).await;
            let mut msg = format!("[cancelled by user; partial output]\nstdout:\n{stdout_str}");
            if !stderr_str.is_empty() {
                msg.push_str(&format!("\nstderr:\n{stderr_str}"));
            }
            Err(msg)
        }
    }
}

/// As [`run_script`], but emits bounded chunks as they arrive.  The final
/// result is deliberately identical, so callers can opt into live rendering
/// without changing model-visible output or cancellation semantics.
pub async fn run_script_live(
    request: ScriptRequest,
    output_events: Option<tokio::sync::mpsc::UnboundedSender<crate::runtime::RuntimeEvent>>,
    call_id: String,
) -> Result<ScriptOutput, String> {
    let timeout_ms = request.timeout_ms.clamp(1_000, 3_600_000);
    let cancel = request.cancel.clone();
    let (shell, shell_arg, _) = shell_command();
    let mut cmd = Command::new(shell);
    cmd.arg(shell_arg)
        .arg(&request.command)
        .envs(request.env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| {
            setsid();
            Ok(())
        });
    }
    let mut child = cmd.spawn().map_err(crate::util::errstr)?;
    let stdout = child.stdout.take().ok_or("failed to capture stdout")?;
    let stderr = child.stderr.take().ok_or("failed to capture stderr")?;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(bool, Vec<u8>)>();
    let tx_out = tx.clone();
    tokio::spawn(async move {
        let mut reader = stdout;
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx_out.send((false, buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
            }
        }
    });
    let tx_err = tx.clone();
    tokio::spawn(async move {
        let mut reader = stderr;
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx_err.send((true, buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
            }
        }
    });
    drop(tx);
    let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut status = None;
    let mut timed_out = false;
    let mut cancelled = false;
    let mut output_open = true;
    loop {
        tokio::select! {
            biased;
            _ = await_cancel(cancel.as_ref()) => { cancelled = true; break; }
            _ = tokio::time::sleep_until(deadline) => { timed_out = true; break; }
            r = child.wait() => { status = Some(r.map_err(crate::util::errstr)?); break; }
            chunk = rx.recv(), if output_open => match chunk {
                Some((is_err, bytes)) => {
                    if let Some(events) = &output_events { let _ = events.send(crate::runtime::RuntimeEvent::ToolOutput { call_id: call_id.clone(), content: String::from_utf8_lossy(&bytes).into_owned(), stderr: is_err }); }
                    if is_err { err.extend(bytes) } else { out.extend(bytes) }
                }
                // Once both pipe readers have stopped, disable this select
                // branch. Otherwise `recv()` would repeatedly resolve to
                // `None` while the child is still running.
                None => output_open = false,
            }
        }
    }
    if timed_out || cancelled {
        #[cfg(unix)]
        if let Some(pid) = child.id() {
            kill_process_group(pid);
        }
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
    while let Some((is_err, bytes)) = rx.recv().await {
        if is_err {
            err.extend(bytes)
        } else {
            out.extend(bytes)
        }
    }
    let stdout = truncate_output(&String::from_utf8_lossy(&out), 500);
    let stderr = truncate_output(&String::from_utf8_lossy(&err), 100);
    if cancelled || timed_out {
        let why = if cancelled {
            "cancelled by user".to_string()
        } else {
            format!("timed out after {timeout_ms}ms")
        };
        let mut msg = format!("[{why}; partial output]\nstdout:\n{stdout}");
        if !stderr.is_empty() {
            msg.push_str(&format!("\nstderr:\n{stderr}"));
        }
        return Err(msg);
    }
    let status = status.ok_or("process ended without status")?;
    Ok(ScriptOutput {
        exit_code: status.code(),
        signal: exit_signal(&status),
        stdout,
        stderr,
    })
}

/// How the child's `wait()` resolved: it exited, the wall-clock timeout fired,
/// or the user cancelled mid-run.
enum WaitOutcome {
    Exited(std::process::ExitStatus),
    TimedOut,
    Cancelled,
}

/// Awaitable cancel: resolves once the shared flag flips, so a `select!` can
/// interrupt `child.wait()` the instant Esc lands rather than only at the next
/// wall-clock boundary. `None` (no flag, e.g. headless `ctx.shell`) never
/// resolves, so the `select!` always takes the wait branch there.
async fn await_cancel(cancel: Option<&Arc<AtomicBool>>) {
    match cancel {
        Some(flag) => {
            while !flag.load(std::sync::atomic::Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }
        None => std::future::pending::<()>().await,
    }
}

/// Kill the child and its whole process tree, reap it, and drain the captured
/// stdout/stderr so partial output survives. Shared by the timeout and cancel
/// paths. The pipes close once the process group dies, so the read tasks
/// started in `run_script` finish and return everything buffered so far.
async fn kill_and_drain(
    child: &mut tokio::process::Child,
    stdout_task: JoinHandle<Vec<u8>>,
    stderr_task: JoinHandle<Vec<u8>>,
) -> (String, String) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        kill_process_group(pid);
    }
    // Cross-platform backstop: signal the direct child even where the group
    // kill above is a no-op (non-Unix) or the pid was already reaped. Ignored
    // errors are fine — the child may already be dead.
    let _ = child.start_kill();
    let _ = child.wait().await;
    let out = stdout_task.await.unwrap_or_default();
    let err = stderr_task.await.unwrap_or_default();
    (
        truncate_output(&String::from_utf8_lossy(&out), 500),
        truncate_output(&String::from_utf8_lossy(&err), 100),
    )
}

/// Deserialize `shell` arguments: the command plus a clamped timeout.
fn parse_shell_args(arguments: Value) -> Result<(String, u64, bool), String> {
    let args: Args = serde_json::from_value(arguments).map_err(crate::util::errstr)?;
    reject_obvious_file_write(&args.command)?;
    let timeout_ms = args.timeout_ms.unwrap_or(120_000).clamp(1_000, 3_600_000);
    Ok((args.command, timeout_ms, args.background))
}

/// Reject unmistakable attempts to use shell as a text-file writer while
/// leaving builds, formatters, generators, bulk transforms, and read fallbacks
/// available. This is intentionally narrower than the prompt guidance.
fn reject_obvious_file_write(command: &str) -> Result<(), String> {
    let trimmed = command.trim_start();
    let first = trimmed
        .split_whitespace()
        .next()
        .unwrap_or("")
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let lower = trimmed.to_ascii_lowercase();

    let sed_in_place = first == "sed"
        && trimmed
            .split_whitespace()
            .skip(1)
            .take_while(|token| token.starts_with('-'))
            .any(|token| token == "-i" || token.starts_with("-i."));
    if sed_in_place {
        return Err(
            "use read_file followed by edit_file instead of `sed -i` for file contents".to_string(),
        );
    }

    if first == "tee" {
        return Err("use write_file for a new file or read_file followed by edit_file for an existing file instead of `tee`".to_string());
    }

    let content_emitter = matches!(first.as_str(), "echo" | "printf" | "cat");
    let redirects_output = lower.contains(" >") || lower.contains(" 1>") || lower.contains(">>");
    let pipes_to_tee = lower.contains("| tee ") || lower.ends_with("| tee");
    if content_emitter && (redirects_output || pipes_to_tee) {
        return Err("use write_file for a new file or read_file followed by edit_file for an existing file instead of shell redirection".to_string());
    }

    Ok(())
}

/// Render a finished command as the tool result the model reads.
fn format_output(output: &ScriptOutput) -> String {
    let mut result = format!(
        "exit code: {}\nstdout:\n{}",
        output_status_label(output),
        output.stdout,
    );
    if !output.stderr.is_empty() {
        result.push_str(&format!("\nstderr:\n{}", output.stderr));
    }
    result
}

/// Truncate output to `max_lines`, keeping the first half and last half with a
/// marker showing how many lines were omitted.
pub fn truncate_output(output: &str, max_lines: usize) -> String {
    let mut lines = Vec::new();
    let mut line_truncated = false;
    for line in output.lines() {
        let truncated = truncate_line(line);
        line_truncated |= truncated.len() != line.len();
        lines.push(truncated);
    }
    if lines.len() <= max_lines {
        return if line_truncated {
            lines.join("\n")
        } else {
            output.to_string()
        };
    }
    let head = max_lines / 2;
    let tail = max_lines - head;
    let truncated = format!("... {} lines truncated ...", lines.len() - max_lines);
    let mut out = Vec::with_capacity(max_lines + 1);
    out.extend(lines.drain(..head));
    out.push(truncated);
    let keep_from = lines.len().saturating_sub(tail);
    out.extend(lines.into_iter().skip(keep_from));
    out.join("\n")
}

#[cfg(unix)]
fn exit_signal(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

#[cfg(not(unix))]
fn exit_signal(_: &std::process::ExitStatus) -> Option<i32> {
    None
}

fn output_status_label(output: &ScriptOutput) -> String {
    if let Some(code) = output.exit_code {
        code.to_string()
    } else if let Some(signal) = output.signal {
        format!("killed by signal {signal}")
    } else {
        "signal".to_string()
    }
}

// ── Shell tool ──────────────────────────────────────────────────────────────

pub struct ShellTool;

#[derive(Deserialize)]
struct Args {
    command: String,
    timeout_ms: Option<u64>,
    #[serde(default)]
    background: bool,
}

#[async_trait]
impl Tool for ShellTool {
    fn definition(&self) -> ToolDefinition {
        let (_, _, shell_label) = shell_command();
        let desc = format!(
            "Run a non-interactive shell command with {shell_label}. Do not use shell to read, create, or edit file contents when read_file, write_file, or edit_file can do it. File-tool fallbacks are appropriate only when a file tool recommends shell, for bulk multi-file operations, or when no dedicated tool supports the operation. Returns exit code, stdout, and stderr."
        );
        let cmd_desc = format!("Command to execute with {shell_label}. Runs without stdin.");
        ToolDefinition {
            name: "shell".to_string(),
            description: desc,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": cmd_desc,
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "minimum": 1000,
                        "description": "Timeout in ms. Default 120000. Set higher for long-running commands (e.g. downloads)."
                    },
                    "background": {
                        "type": "boolean",
                        "description": "Run as a managed background process and return its process id immediately."
                    }
                },
                "required": ["command"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(&self, arguments: Value) -> Result<String, String> {
        // Context-less fallback (trait default path / tests). No cancel token
        // is available here, so the wall-clock timeout is the only backstop;
        // the live path below wires in cancellation.
        let (command, timeout_ms, background) = parse_shell_args(arguments)?;
        if background {
            let id = crate::processes::registry().spawn(command, "shell".into(), timeout_ms);
            return Ok(format!("background process started: {id}"));
        }
        let output = run_script(ScriptRequest {
            command,
            env: Vec::new(),
            timeout_ms,
            cancel: None,
        })
        .await?;
        Ok(format_output(&output))
    }

    async fn execute_output_live(
        &self,
        arguments: Value,
        _events: Option<tokio::sync::mpsc::UnboundedSender<crate::pane_content::KeyRequest>>,
        context: ToolExecutionContext,
    ) -> Result<ToolOutput, String> {
        // Live path used by the driver: thread the turn's cancel flag in so an
        // Esc mid-command kills the process tree and returns promptly instead
        // of blocking until the wall-clock timeout.
        let (command, timeout_ms, background) = parse_shell_args(arguments)?;
        if background {
            let id = crate::processes::registry().spawn(command, context.owner.clone(), timeout_ms);
            return Ok(ToolOutput::text(format!(
                "background process started: {id}"
            )));
        }
        let output = run_script_live(
            ScriptRequest {
                command,
                env: Vec::new(),
                timeout_ms,
                cancel: context.cancelled.clone(),
            },
            context.runtime_events.clone(),
            context.call_id.clone(),
        )
        .await?;
        Ok(ToolOutput::text(format_output(&output)))
    }
}

//! The `shell` / `bash` tool: runs commands with streaming output and timeouts.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time::Duration;

use crate::tools::types::{Tool, ToolDefinition, ToolExecutionContext, ToolOutput};
use crate::tools::{MAX_TOOL_LINE_CHARS, truncate_line};

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

#[cfg(windows)]
async fn kill_process_tree(pid: u32) {
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
}

pub struct ScriptRequest {
    pub command: String,
    pub env: Vec<(String, String)>,
    pub timeout_ms: u64,
    pub working_dir: Option<PathBuf>,
    /// Cooperative cancel flag. When set (Esc/Ctrl+C mid-turn), the executor
    /// kills the process tree and returns promptly with partial output instead
    /// of blocking until `timeout_ms`. `None` only for context-less callers.
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

fn spawn_script(
    request: ScriptRequest,
) -> Result<
    (
        tokio::process::Child,
        tokio::process::ChildStdout,
        tokio::process::ChildStderr,
    ),
    String,
> {
    if request.command.contains('\0') {
        return Err("shell command must not contain NUL bytes".to_string());
    }
    let (shell, shell_arg, _) = shell_command();
    let mut cmd = Command::new(shell);
    cmd.arg(shell_arg)
        .arg(request.command)
        .envs(request.env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(working_dir) = request.working_dir {
        cmd.current_dir(working_dir);
    }
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
    let stdout = child.stdout.take().ok_or("failed to capture stdout")?;
    let stderr = child.stderr.take().ok_or("failed to capture stderr")?;
    Ok((child, stdout, stderr))
}

const CAPTURE_BYTES: usize = 2 * 1024 * 1024;

/// Keep command output memory bounded while preserving both useful ends.
struct OutputCapture {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    omitted: usize,
}

impl OutputCapture {
    fn new() -> Self {
        Self {
            head: Vec::with_capacity(CAPTURE_BYTES / 2),
            tail: VecDeque::with_capacity(CAPTURE_BYTES / 2),
            omitted: 0,
        }
    }

    fn push(&mut self, mut bytes: &[u8]) {
        let head_room = CAPTURE_BYTES / 2 - self.head.len();
        let keep = head_room.min(bytes.len());
        self.head.extend_from_slice(&bytes[..keep]);
        bytes = &bytes[keep..];

        let tail_limit = CAPTURE_BYTES / 2;
        if bytes.len() >= tail_limit {
            self.omitted += self.tail.len() + bytes.len() - tail_limit;
            self.tail.clear();
            bytes = &bytes[bytes.len() - tail_limit..];
        } else {
            let overflow = (self.tail.len() + bytes.len()).saturating_sub(tail_limit);
            self.omitted += overflow;
            self.tail.drain(..overflow);
        }
        self.tail.extend(bytes.iter().copied());
    }

    fn render(self, max_lines: usize) -> String {
        let marker = format!("\n... {} bytes truncated ...\n", self.omitted);
        let mut bytes = Vec::with_capacity(
            self.head.len() + self.tail.len() + usize::from(self.omitted > 0) * marker.len(),
        );
        bytes.extend(self.head);
        if self.omitted > 0 {
            bytes.extend(marker.as_bytes());
        }
        bytes.extend(self.tail);
        truncate_output(&String::from_utf8_lossy(&bytes), max_lines)
    }
}

pub async fn run_script(request: ScriptRequest) -> Result<ScriptOutput, String> {
    run_script_stream(request, |_, _| Ok(())).await
}

/// Run a script while observing each stdout/stderr chunk. The shared executor
/// owns timeout, cancellation, process-tree cleanup, reaping, and final output.
pub(crate) async fn run_script_stream<F>(
    request: ScriptRequest,
    mut emit: F,
) -> Result<ScriptOutput, String>
where
    F: FnMut(bool, &[u8]) -> Result<(), String>,
{
    let timeout_ms = request.timeout_ms.clamp(1_000, 3_600_000);
    let cancel = request.cancel.clone();
    let (mut child, stdout, stderr) = spawn_script(request)?;
    // A small bounded queue lets slow consumers apply backpressure to both OS
    // pipes instead of buffering arbitrary command output in the daemon.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(bool, Vec<u8>)>(16);
    let tx_out = tx.clone();
    tokio::spawn(async move {
        let mut reader = stdout;
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx_out.send((false, buf[..n].to_vec())).await.is_err() {
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
                    if tx_err.send((true, buf[..n].to_vec())).await.is_err() {
                        break;
                    }
                }
            }
        }
    });
    drop(tx);
    let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
    let mut out = OutputCapture::new();
    let mut err = OutputCapture::new();
    let mut status = None;
    let mut timed_out = false;
    let mut cancelled = false;
    let mut emit_error = None;
    let mut output_open = true;
    loop {
        tokio::select! {
            biased;
            _ = await_cancel(cancel.as_ref()) => { cancelled = true; break; }
            _ = tokio::time::sleep_until(deadline) => { timed_out = true; break; }
            r = child.wait() => { status = Some(r.map_err(crate::util::errstr)?); break; }
            chunk = rx.recv(), if output_open => match chunk {
                Some((is_err, bytes)) => {
                    if is_err { err.push(&bytes) } else { out.push(&bytes) }
                    if let Err(error) = emit(is_err, &bytes) {
                        emit_error = Some(error);
                        break;
                    }
                }
                // Once both pipe readers have stopped, disable this select
                // branch. Otherwise `recv()` would repeatedly resolve to
                // `None` while the child is still running.
                None => output_open = false,
            }
        }
    }
    if timed_out || cancelled || emit_error.is_some() {
        #[cfg(unix)]
        if let Some(pid) = child.id() {
            kill_process_group(pid);
        }
        #[cfg(windows)]
        if let Some(pid) = child.id() {
            kill_process_tree(pid).await;
        }
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
    while let Some((is_err, bytes)) = rx.recv().await {
        if is_err {
            err.push(&bytes)
        } else {
            out.push(&bytes)
        }
        if !timed_out
            && !cancelled
            && emit_error.is_none()
            && let Err(error) = emit(is_err, &bytes)
        {
            emit_error = Some(error);
        }
    }
    let stdout = out.render(500);
    let stderr = err.render(100);
    if let Some(error) = emit_error {
        return Err(error);
    }
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

/// As [`run_script`], but emits bounded chunks as they arrive. The final
/// result is deliberately identical, so callers can opt into live rendering
/// without changing model-visible output or cancellation semantics.
pub async fn run_script_live(
    request: ScriptRequest,
    output_events: Option<tokio::sync::mpsc::UnboundedSender<crate::runtime::RuntimeEvent>>,
    call_id: String,
) -> Result<ScriptOutput, String> {
    let mut emitted = 0usize;
    let mut truncation_sent = false;
    run_script_stream(request, |is_err, bytes| {
        let Some(events) = &output_events else {
            return Ok(());
        };
        let keep = (CAPTURE_BYTES - emitted).min(bytes.len());
        if keep > 0 {
            emitted += keep;
            let _ = events.send(crate::runtime::RuntimeEvent::ToolOutput {
                call_id: call_id.clone(),
                content: String::from_utf8_lossy(&bytes[..keep]).into_owned(),
                stderr: is_err,
            });
        }
        if keep < bytes.len() && !truncation_sent {
            truncation_sent = true;
            let _ = events.send(crate::runtime::RuntimeEvent::ToolOutput {
                call_id: call_id.clone(),
                content: format!(
                    "\n... live shell output truncated after {CAPTURE_BYTES} bytes ...\n"
                ),
                stderr: is_err,
            });
        }
        Ok(())
    })
    .await
}

fn render_stream_line(bytes: &[u8], was_truncated: bool) -> String {
    let mut line = truncate_line(&String::from_utf8_lossy(bytes));
    if was_truncated && !line.ends_with("…[truncated]") {
        line.push_str("…[truncated]");
    }
    line
}

/// As [`run_script`], but invokes `callback` for each complete stdout line.
/// Callback failures stop and reap the whole process tree before returning.
pub async fn run_script_lines<F>(
    request: ScriptRequest,
    mut callback: F,
) -> Result<ScriptOutput, String>
where
    F: FnMut(String) -> Result<(), String>,
{
    const MAX_LINE_BYTES: usize = MAX_TOOL_LINE_CHARS * 4;

    let mut pending = Vec::new();
    let mut pending_truncated = false;
    let result = run_script_stream(request, |is_err, bytes| {
        if is_err {
            return Ok(());
        }
        for chunk in bytes.split_inclusive(|byte| *byte == b'\n') {
            let complete = chunk.ends_with(b"\n");
            let content = if complete {
                &chunk[..chunk.len() - 1]
            } else {
                chunk
            };
            let keep = (MAX_LINE_BYTES - pending.len()).min(content.len());
            pending.extend_from_slice(&content[..keep]);
            pending_truncated |= keep < content.len();
            if complete {
                if pending.last() == Some(&b'\r') {
                    pending.pop();
                }
                callback(render_stream_line(&pending, pending_truncated))?;
                pending.clear();
                pending_truncated = false;
            }
        }
        Ok(())
    })
    .await;
    match result {
        Ok(output) => {
            if !pending.is_empty() || pending_truncated {
                callback(render_stream_line(&pending, pending_truncated))?;
            }
            Ok(output)
        }
        Err(error) => Err(error),
    }
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

/// Deserialize `shell` arguments. Calls without an action remain `run` for
/// compatibility with existing transcripts and clients.
fn parse_shell_args(arguments: Value) -> Result<Args, String> {
    serde_json::from_value(arguments).map_err(crate::util::errstr)
}

fn parse_run_args(args: Args) -> Result<(String, u64, bool), String> {
    let command = args.command.ok_or("command is required for run")?;
    reject_obvious_file_write(&command)?;
    let timeout_ms = args.timeout_ms.unwrap_or(120_000).clamp(1_000, 3_600_000);
    Ok((command, timeout_ms, args.background))
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
            .any(|token| token.starts_with("-i") || token.starts_with("--in-place"));
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
    #[serde(default = "default_action")]
    action: String,
    #[serde(default)]
    command: Option<String>,
    timeout_ms: Option<u64>,
    #[serde(default)]
    background: bool,
    #[serde(default)]
    id: Option<String>,
}

fn default_action() -> String {
    "run".into()
}

#[async_trait]
impl Tool for ShellTool {
    fn definition(&self) -> ToolDefinition {
        let (_, _, shell_label) = shell_command();
        let desc = format!(
            "Run a non-interactive shell command with {shell_label}, or manage background commands started by this tool. Use action=run (the default), list, status, or kill. Do not use shell to read, create, or edit file contents when read_file, write_file, or edit_file can do it. File-tool fallbacks are appropriate only when a file tool recommends shell, for bulk multi-file operations, or when no dedicated tool supports the operation. Run returns exit code, stdout, and stderr."
        );
        let cmd_desc =
            format!("Command to execute with {shell_label} for action=run. Runs without stdin.");
        ToolDefinition {
            name: "shell".to_string(),
            description: desc,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["run", "list", "status", "kill"],
                        "description": "Action to perform. Defaults to run when command is provided."
                    },
                    "command": {
                        "type": "string",
                        "description": cmd_desc,
                    },
                    "id": {
                        "type": "string",
                        "description": "Managed process id for status or kill."
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "minimum": 1000,
                        "description": "Timeout in ms for run. Default 120000. Set higher for long-running commands (e.g. downloads)."
                    },
                    "background": {
                        "type": "boolean",
                        "description": "For run, start a managed background process and return its id immediately."
                    }
                },
                "additionalProperties": false
            }),
        }
    }

    async fn execute(&self, arguments: Value) -> Result<String, String> {
        // Context-less fallback (trait default path / tests). No cancel token
        // is available here, so the wall-clock timeout is the only backstop;
        // the live path below wires in cancellation.
        let args = parse_shell_args(arguments)?;
        if args.action != "run" {
            return crate::processes::execute_action(&args.action, args.id.as_deref(), None);
        }
        let (command, timeout_ms, background) = parse_run_args(args)?;
        if background {
            let id = crate::processes::registry().spawn(command, "shell".into(), timeout_ms, None);
            return Ok(format!("background process started: {id}"));
        }
        let output = run_script(ScriptRequest {
            command,
            env: Vec::new(),
            timeout_ms,
            working_dir: None,
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
        let args = parse_shell_args(arguments)?;
        let scope = crate::processes::conversation_scope(
            context
                .app_state
                .as_ref()
                .and_then(|state| state.session_id),
        );
        if args.action != "run" {
            return crate::processes::execute_action(
                &args.action,
                args.id.as_deref(),
                Some(&scope),
            )
            .map(ToolOutput::text);
        }
        let (command, timeout_ms, background) = parse_run_args(args)?;
        if background {
            let id = crate::processes::registry().spawn(
                command,
                scope,
                timeout_ms,
                context.working_dir.clone(),
            );
            return Ok(ToolOutput::text(format!(
                "background process started: {id}"
            )));
        }
        let output = run_script_live(
            ScriptRequest {
                command,
                env: Vec::new(),
                timeout_ms,
                working_dir: context.working_dir.clone(),
                cancel: context.cancelled.clone(),
            },
            context.runtime_events.clone(),
            context.call_id.clone(),
        )
        .await?;
        Ok(ToolOutput::text(format_output(&output)))
    }
}

#[cfg(test)]
mod capture_tests {
    use super::*;

    #[test]
    fn output_capture_keeps_both_ends_with_a_fixed_memory_limit() {
        let mut capture = OutputCapture::new();
        capture.push(&vec![b'a'; CAPTURE_BYTES]);
        capture.push(&vec![b'z'; CAPTURE_BYTES]);

        let output = capture.render(500);
        assert!(output.starts_with('a'));
        assert!(
            output
                .lines()
                .last()
                .is_some_and(|line| line.starts_with('z'))
        );
        assert!(output.contains("bytes truncated"));
        assert!(output.len() < 10_000);
    }
}

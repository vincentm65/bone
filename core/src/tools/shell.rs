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
fn kill_process_group(pid: u32) {
    unsafe {
        let _ = kill(-(pid as i32), 9);
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

pub struct DirectExecRequest {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub stdin: Option<Vec<u8>>,
    pub working_dir: Option<PathBuf>,
    pub timeout_ms: u64,
    pub cancel: Option<Arc<AtomicBool>>,
    pub max_output_bytes: usize,
}

pub struct DirectExecOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub timed_out: bool,
    pub cancelled: bool,
    pub output_limit_exceeded: bool,
}

pub struct DirectExecError {
    pub spawned: bool,
    pub message: String,
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

struct ProcessRequest {
    program: String,
    args: Vec<String>,
    env: Vec<(String, String)>,
    stdin: Option<Vec<u8>>,
    working_dir: Option<PathBuf>,
    timeout_ms: u64,
    cancel: Option<Arc<AtomicBool>>,
}

struct ProcessOutput {
    exit_code: Option<i32>,
    signal: Option<i32>,
    timed_out: bool,
    cancelled: bool,
    output_limit_exceeded: bool,
}

enum StreamError {
    OutputLimit,
    Other(String),
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

async fn run_process_stream<F>(
    request: ProcessRequest,
    mut emit: F,
) -> Result<ProcessOutput, DirectExecError>
where
    F: FnMut(bool, &[u8]) -> Result<(), StreamError>,
{
    let mut command = Command::new(request.program);
    command
        .args(request.args)
        .envs(request.env)
        .stdin(if request.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(working_dir) = request.working_dir {
        command.current_dir(working_dir);
    }
    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            setsid();
            Ok(())
        });
    }
    let mut child = command.spawn().map_err(|error| DirectExecError {
        spawned: false,
        message: error.to_string(),
    })?;
    let pid = child.id().ok_or_else(|| DirectExecError {
        spawned: true,
        message: "failed to obtain child process id".into(),
    })?;
    if let Some(input) = request.stdin
        && let Some(mut stdin) = child.stdin.take()
    {
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let _ = stdin.write_all(&input).await;
        });
    }
    let stdout = child.stdout.take().ok_or_else(|| DirectExecError {
        spawned: true,
        message: "failed to capture stdout".into(),
    })?;
    let stderr = child.stderr.take().ok_or_else(|| DirectExecError {
        spawned: true,
        message: "failed to capture stderr".into(),
    })?;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(bool, Vec<u8>)>(16);
    let mut readers = Vec::with_capacity(2);
    for (is_stderr, mut reader) in [
        (
            false,
            Box::new(stdout) as Box<dyn tokio::io::AsyncRead + Unpin + Send>,
        ),
        (true, Box::new(stderr)),
    ] {
        let tx = tx.clone();
        readers.push(tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) if tx.send((is_stderr, buf[..n].to_vec())).await.is_err() => break,
                    Ok(_) => {}
                }
            }
        }));
    }
    drop(tx);

    let deadline = tokio::time::Instant::now() + Duration::from_millis(request.timeout_ms);
    let mut status = None;
    let mut output_open = true;
    let mut timed_out = false;
    let mut cancelled = false;
    let mut stream_error = None;
    loop {
        if status.is_some() && !output_open {
            break;
        }
        tokio::select! {
            biased;
            _ = await_cancel(request.cancel.as_ref()) => { cancelled = true; break; }
            _ = tokio::time::sleep_until(deadline) => { timed_out = true; break; }
            result = child.wait(), if status.is_none() => {
                status = Some(result.map_err(|error| DirectExecError {
                    spawned: true,
                    message: error.to_string(),
                })?);
            }
            chunk = rx.recv(), if output_open => match chunk {
                Some((is_stderr, bytes)) => if let Err(error) = emit(is_stderr, &bytes) {
                    stream_error = Some(error);
                    break;
                },
                None => output_open = false,
            }
        }
    }
    if timed_out || cancelled || stream_error.is_some() {
        #[cfg(unix)]
        kill_process_group(pid);
        #[cfg(windows)]
        kill_process_tree(pid).await;
        let _ = child.start_kill();
        if status.is_none() {
            status = Some(child.wait().await.map_err(|error| DirectExecError {
                spawned: true,
                message: error.to_string(),
            })?);
        }
    }
    if stream_error.is_none() {
        // After cancellation/timeout, do not wait for pipe EOF: a descendant
        // may have escaped the killed process group while retaining stdout or
        // stderr, which would wedge the cancelled turn here indefinitely.
        while let Ok((is_stderr, bytes)) = rx.try_recv() {
            if let Err(error) = emit(is_stderr, &bytes) {
                stream_error = Some(error);
                break;
            }
        }
        if !timed_out && !cancelled {
            while let Some((is_stderr, bytes)) = rx.recv().await {
                if let Err(error) = emit(is_stderr, &bytes) {
                    stream_error = Some(error);
                    break;
                }
            }
        }
    }
    if timed_out || cancelled || stream_error.is_some() {
        for reader in readers {
            reader.abort();
        }
    }
    let output_limit_exceeded = matches!(stream_error, Some(StreamError::OutputLimit));
    if let Some(StreamError::Other(message)) = stream_error {
        return Err(DirectExecError {
            spawned: true,
            message,
        });
    }
    let status = status.ok_or_else(|| DirectExecError {
        spawned: true,
        message: "process ended without status".into(),
    })?;
    Ok(ProcessOutput {
        exit_code: status.code(),
        signal: exit_signal(&status),
        timed_out,
        cancelled,
        output_limit_exceeded,
    })
}

pub async fn run_direct_exec(
    request: DirectExecRequest,
) -> Result<DirectExecOutput, DirectExecError> {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut total = 0usize;
    let max_output_bytes = request.max_output_bytes;
    let output = run_process_stream(
        ProcessRequest {
            program: request.program,
            args: request.args,
            env: request.env,
            stdin: request.stdin,
            working_dir: request.working_dir,
            timeout_ms: request.timeout_ms,
            cancel: request.cancel,
        },
        |is_stderr, bytes| {
            total = total.saturating_add(bytes.len());
            if total > max_output_bytes {
                return Err(StreamError::OutputLimit);
            }
            if is_stderr {
                stderr.extend_from_slice(bytes);
            } else {
                stdout.extend_from_slice(bytes);
            }
            Ok(())
        },
    )
    .await?;
    Ok(DirectExecOutput {
        stdout,
        stderr,
        exit_code: output.exit_code,
        signal: output.signal,
        timed_out: output.timed_out,
        cancelled: output.cancelled,
        output_limit_exceeded: output.output_limit_exceeded,
    })
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
    if request.command.contains('\0') {
        return Err("shell command must not contain NUL bytes".into());
    }
    let timeout_ms = request.timeout_ms.clamp(1_000, 3_600_000);
    let cancel = request.cancel.clone();
    let (shell, shell_arg, _) = shell_command();
    let mut out = OutputCapture::new();
    let mut err = OutputCapture::new();
    let output = run_process_stream(
        ProcessRequest {
            program: shell.into(),
            args: vec![shell_arg.into(), request.command],
            env: request.env,
            stdin: None,
            working_dir: request.working_dir,
            timeout_ms,
            cancel: request.cancel,
        },
        |is_stderr, bytes| {
            if is_stderr {
                err.push(bytes)
            } else {
                out.push(bytes)
            }
            // Buffered pipe bytes are still captured for the final partial
            // output, but cancellation must stop observable callbacks.
            if cancel
                .as_ref()
                .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed))
            {
                Ok(())
            } else {
                emit(is_stderr, bytes).map_err(StreamError::Other)
            }
        },
    )
    .await
    .map_err(|error| error.message)?;
    let stdout = out.render(500);
    let stderr = err.render(100);
    if output.cancelled || output.timed_out {
        let why = if output.cancelled {
            "cancelled by user".to_string()
        } else {
            format!("timed out after {timeout_ms}ms")
        };
        let mut message = format!("[{why}; partial output]\nstdout:\n{stdout}");
        if !stderr.is_empty() {
            message.push_str(&format!("\nstderr:\n{stderr}"));
        }
        return Err(message);
    }
    Ok(ScriptOutput {
        exit_code: output.exit_code,
        signal: output.signal,
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

    let cancel = request.cancel.clone();
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
                // The cancel flag can flip while an earlier callback is
                // running. Check between complete lines so a second line from
                // the same OS read is not delivered after cancellation.
                if !cancel
                    .as_ref()
                    .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed))
                {
                    callback(render_stream_line(&pending, pending_truncated))?;
                }
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
        return Err("use create_file for a new file or read_file followed by edit_file for an existing file instead of `tee`".to_string());
    }

    let content_emitter = matches!(first.as_str(), "echo" | "printf" | "cat");
    let redirects_output = lower.contains(" >") || lower.contains(" 1>") || lower.contains(">>");
    let pipes_to_tee = lower.contains("| tee ") || lower.ends_with("| tee");
    if content_emitter && (redirects_output || pipes_to_tee) {
        return Err("use create_file for a new file or read_file followed by edit_file for an existing file instead of shell redirection".to_string());
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
            "Run a non-interactive shell command with {shell_label}, or manage background commands started by this tool. Use action=run (the default), list, status, or kill. Do not use shell to read, create, or edit file contents when read_file, create_file, or edit_file can do it. File-tool fallbacks are appropriate only when a file tool recommends shell, for bulk multi-file operations, or when no dedicated tool supports the operation. Run returns exit code, stdout, and stderr."
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
        let scope = context.app_state.as_ref().map_or_else(
            || crate::processes::conversation_scope(None),
            |state| {
                crate::processes::conversation_scope(state.background_scope.or(state.session_id))
            },
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

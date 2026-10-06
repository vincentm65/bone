//! The model-facing shell and its managed process registry.
//!
//! A foreground command is just a job that is waited on. A background
//! command is the same job with its id returned to the model.

use std::collections::HashMap;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::pipe;
use tokio::sync::{Notify, mpsc};
use tokio::time::{Instant, sleep_until};
use tokio_util::sync::CancellationToken;

use super::{
    OutputSink, ProcessUpdateSink, Tool, ToolContext, ToolResult, ToolSpec, truncate_middle,
    typed_args,
};

const DEFAULT_TIMEOUT_SECS: u64 = 120;
const MAX_TIMEOUT_SECS: u64 = 600;
const EXIT_GRACE: Duration = Duration::from_millis(200);
const KILL_GRACE: Duration = Duration::from_secs(2);
const MAX_OUTPUT: usize = 4 << 20;
const SHOW_HEAD: usize = 10_000;
const SHOW_TAIL: usize = 20_000;
const MAX_COMPLETED: usize = 64;
/// Output kept as written, escape codes and all, for terminal views.
const MAX_RAW: usize = 1 << 20;
/// A background job's terminal until a client sets another size.
const PTY_COLS: u16 = 120;
const PTY_ROWS: u16 = 32;
const TAIL_MAX: usize = 200;

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

pub struct Shell(ToolSpec);

impl Shell {
    pub fn new() -> Self {
        Shell(ToolSpec {
            name: "shell".into(),
            description: "Run a bash command, stream its output, or manage a command started in the background. Use mode=\"start\" for long-lived commands (they run in a terminal the user can watch), then use action=status, read, wait, or kill with its job id. Commands are non-interactive unless stdin is supplied.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["run", "start", "list", "status", "read", "wait", "kill", "write", "close_stdin"], "description": "run (default), start, or a job-management action." },
                    "command": { "type": "string", "description": "The bash command for run or start." },
                    "mode": { "type": "string", "enum": ["wait", "start"], "description": "wait (default) for a foreground command, or start and return a job id immediately." },
                    "background": { "type": "boolean", "description": "Compatibility alias for mode=start." },
                    "id": { "type": "string", "description": "Job id for status, read, wait, kill, write, or close_stdin." },
                    "timeout_secs": { "type": "integer", "minimum": 1, "maximum": MAX_TIMEOUT_SECS, "description": "Command timeout. Foreground defaults to 120 seconds; background jobs have no timeout unless set." },
                    "data": { "type": "string", "description": "Data to write to a job opened with stdin=\"pipe\"." },
                    "stdin": { "type": "string", "description": "Input to write before closing stdin." }
                },
                "anyOf": [
                    { "required": ["command"] },
                    { "required": ["action"] }
                ]
            }),
        })
    }
}

#[derive(Deserialize)]
struct Args {
    #[serde(default = "default_action")]
    action: String,
    command: Option<String>,
    mode: Option<String>,
    #[serde(default)]
    background: bool,
    id: Option<String>,
    timeout_secs: Option<u64>,
    data: Option<String>,
    stdin: Option<String>,
}

fn default_action() -> String {
    "run".into()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProcessState {
    Running,
    Exited,
    Cancelled,
    TimedOut,
    Failed,
}

impl ProcessState {
    fn name(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Exited => "exited",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone)]
struct Snapshot {
    id: String,
    owner: String,
    command: String,
    state: ProcessState,
    pid: Option<u32>,
    started: Instant,
    finished: Option<Instant>,
    started_at_ms: u64,
    finished_at_ms: Option<u64>,
    /// What the model reads: for a terminal job, escape codes removed and
    /// `\r` progress lines settled.
    combined: String,
    /// The output as written, from byte `raw_start` of `raw_total`.
    raw: String,
    raw_start: u64,
    raw_total: u64,
    output_bytes: u64,
    truncated: bool,
    background: bool,
    timeout_secs: Option<u64>,
    code: Option<i32>,
    signal: Option<i32>,
    error: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct ProcessView {
    pub id: String,
    pub command: String,
    pub state: ProcessState,
    pub running: bool,
    pub pid: Option<u32>,
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
    pub elapsed_ms: u64,
    pub tail: String,
    pub output_bytes: u64,
    pub truncated: bool,
    pub code: Option<i32>,
    pub signal: Option<i32>,
    pub error: Option<String>,
    pub terminal: bool,
    /// New output `(offset, text)`, when that is what changed.
    pub chunk: Option<(u64, String)>,
}

struct Process {
    snapshot: Mutex<Snapshot>,
    /// The pseudo-terminal's master side while a terminal job runs.
    pty: Mutex<Option<OwnedFd>>,
    control: mpsc::UnboundedSender<Control>,
    done: Notify,
}

enum Control {
    Write(Vec<u8>),
    CloseStdin,
    Cancel,
}

struct JobSpec {
    owner: String,
    command: String,
    cwd: PathBuf,
    timeout: Option<Duration>,
    stdin: Stdin,
    background: bool,
}

struct JobHooks {
    parent_cancel: CancellationToken,
    output: Option<OutputSink>,
    process: Option<ProcessUpdateSink>,
}

/// Session-scoped managed commands. Completed jobs remain available for
/// inspection until the bounded registry prunes them.
#[derive(Clone)]
pub(crate) struct ProcessRegistry {
    next: Arc<AtomicU64>,
    version: Arc<AtomicU64>,
    processes: Arc<Mutex<HashMap<String, Arc<Process>>>>,
}

impl Default for ProcessRegistry {
    fn default() -> Self {
        Self {
            next: Arc::new(AtomicU64::new(1)),
            version: Arc::new(AtomicU64::new(0)),
            processes: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl ProcessRegistry {
    fn spawn(&self, spec: JobSpec, hooks: JobHooks) -> String {
        let id = format!("shell-{}", self.next.fetch_add(1, Ordering::Relaxed));
        let (control, controls) = mpsc::unbounded_channel();
        let process = Arc::new(Process {
            snapshot: Mutex::new(Snapshot {
                id: id.clone(),
                owner: spec.owner.clone(),
                command: spec.command.clone(),
                state: ProcessState::Running,
                pid: None,
                started: Instant::now(),
                finished: None,
                started_at_ms: unix_ms(),
                finished_at_ms: None,
                combined: String::new(),
                raw: String::new(),
                raw_start: 0,
                raw_total: 0,
                output_bytes: 0,
                truncated: false,
                timeout_secs: spec.timeout.map(|t| t.as_secs()),
                background: spec.background,
                code: None,
                signal: None,
                error: None,
            }),
            pty: Mutex::new(None),
            control,
            done: Notify::new(),
        });
        self.processes
            .lock()
            .unwrap()
            .insert(id.clone(), process.clone());
        let version = self.bump_version();
        if let Some(sink) = &hooks.process {
            sink(self.view_of(&process), version);
        }
        let registry = self.clone();
        tokio::spawn(async move {
            run_job(registry, process, spec, controls, hooks).await;
        });
        id
    }

    async fn wait(
        &self,
        owner: &str,
        id: &str,
        timeout: Option<Duration>,
    ) -> Result<Snapshot, String> {
        let process = self.get(owner, id)?;
        let deadline = timeout.map(|t| Instant::now() + t);
        loop {
            if process.snapshot.lock().unwrap().state != ProcessState::Running {
                return Ok(process.snapshot.lock().unwrap().clone());
            }
            let notified = process.done.notified();
            match deadline {
                Some(deadline) => {
                    tokio::select! { _ = notified => {}, _ = sleep_until(deadline) => return Err(format!("waiting for {id} timed out")) }
                }
                None => notified.await,
            }
        }
    }

    fn get(&self, owner: &str, id: &str) -> Result<Arc<Process>, String> {
        self.processes
            .lock()
            .unwrap()
            .get(id)
            .filter(|p| p.snapshot.lock().unwrap().owner == owner)
            .cloned()
            .ok_or_else(|| format!("unknown shell job {id:?}"))
    }

    fn snapshot(&self, owner: &str, id: &str) -> Result<Snapshot, String> {
        Ok(self.get(owner, id)?.snapshot.lock().unwrap().clone())
    }

    fn list(&self, owner: &str) -> Vec<Snapshot> {
        let mut jobs: Vec<_> = self
            .processes
            .lock()
            .unwrap()
            .values()
            .filter(|p| p.snapshot.lock().unwrap().owner == owner)
            .filter(|p| p.snapshot.lock().unwrap().background)
            .map(|p| p.snapshot.lock().unwrap().clone())
            .collect();
        jobs.sort_by(|a, b| a.id.cmp(&b.id));
        jobs
    }

    pub(crate) fn cancel(&self, owner: &str, id: &str) -> Result<(), String> {
        self.get(owner, id)?
            .control
            .send(Control::Cancel)
            .map_err(|_| "job is no longer running".into())
    }

    fn bump_version(&self) -> u64 {
        self.version.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub(crate) fn version(&self) -> u64 {
        self.version.load(Ordering::Relaxed)
    }

    fn view_of(&self, process: &Process) -> ProcessView {
        let snapshot = process.snapshot.lock().unwrap();
        ProcessView {
            id: snapshot.id.clone(),
            command: snapshot.command.clone(),
            state: snapshot.state,
            running: snapshot.state == ProcessState::Running,
            pid: snapshot.pid,
            started_at_ms: snapshot.started_at_ms,
            finished_at_ms: snapshot.finished_at_ms,
            elapsed_ms: snapshot.finished.map_or_else(
                || snapshot.started.elapsed().as_millis() as u64,
                |finished| finished.duration_since(snapshot.started).as_millis() as u64,
            ),
            tail: tail_of(&snapshot.combined),
            output_bytes: snapshot.output_bytes,
            truncated: snapshot.truncated,
            code: snapshot.code,
            signal: snapshot.signal,
            error: snapshot.error.clone(),
            terminal: snapshot.background,
            chunk: None,
        }
    }

    /// Output as written from byte `from` on: `(offset, data, total)`.
    pub(crate) fn read(
        &self,
        owner: &str,
        id: &str,
        from: u64,
    ) -> Result<(u64, String, u64), String> {
        let s = self.snapshot(owner, id)?;
        let mut at = from.saturating_sub(s.raw_start).min(s.raw.len() as u64) as usize;
        while !s.raw.is_char_boundary(at) {
            at += 1;
        }
        Ok((s.raw_start + at as u64, s.raw[at..].to_owned(), s.raw_total))
    }

    /// Resize a terminal job's pseudo-terminal.
    pub(crate) fn resize(&self, owner: &str, id: &str, cols: u16, rows: u16) -> Result<(), String> {
        let process = self.get(owner, id)?;
        let pty = process.pty.lock().unwrap();
        let Some(master) = pty.as_ref() else {
            return Err(format!("{id} has no terminal now"));
        };
        let ws = libc::winsize {
            ws_row: rows.max(1),
            ws_col: cols.max(1),
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        if unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &ws) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        // Without a controlling terminal nothing else tells it.
        signal_group(process.snapshot.lock().unwrap().pid, libc::SIGWINCH);
        Ok(())
    }

    pub(crate) fn views(&self, owner: &str) -> Vec<ProcessView> {
        let processes: Vec<_> = self
            .processes
            .lock()
            .unwrap()
            .values()
            .filter(|p| p.snapshot.lock().unwrap().owner == owner)
            .cloned()
            .collect();
        let mut views: Vec<_> = processes.iter().map(|p| self.view_of(p)).collect();
        views.sort_by(|a, b| a.id.cmp(&b.id));
        views
    }
    fn write(&self, owner: &str, id: &str, data: Vec<u8>) -> Result<(), String> {
        self.get(owner, id)?
            .control
            .send(Control::Write(data))
            .map_err(|_| "job is no longer running".into())
    }
    fn close_stdin(&self, owner: &str, id: &str) -> Result<(), String> {
        self.get(owner, id)?
            .control
            .send(Control::CloseStdin)
            .map_err(|_| "job is no longer running".into())
    }

    pub(crate) fn cancel_owner(&self, owner: &str) {
        for process in self.processes.lock().unwrap().values() {
            if process.snapshot.lock().unwrap().owner == owner {
                let _ = process.control.send(Control::Cancel);
            }
        }
    }

    pub(crate) fn cancel_all(&self) {
        for process in self.processes.lock().unwrap().values() {
            let _ = process.control.send(Control::Cancel);
        }
    }

    fn prune(&self) {
        let mut jobs = self.processes.lock().unwrap();
        let mut finished: Vec<_> = jobs
            .iter()
            .filter_map(|(id, p)| {
                (p.snapshot.lock().unwrap().state != ProcessState::Running).then_some(id.clone())
            })
            .collect();
        finished.sort();
        while finished.len() > MAX_COMPLETED {
            jobs.remove(&finished.remove(0));
        }
    }
}

#[derive(Clone)]
enum Stdin {
    Null,
    Text(String),
    Pipe,
}

#[derive(Default)]
struct Decoder {
    tail: Vec<u8>,
}

impl Decoder {
    fn decode(&mut self, bytes: &[u8]) -> String {
        self.tail.extend_from_slice(bytes);
        let valid = match std::str::from_utf8(&self.tail) {
            Ok(_) => self.tail.len(),
            Err(e) if e.error_len().is_none() => e.valid_up_to(),
            Err(_) => self.tail.len(),
        };
        let rest = self.tail.split_off(valid);
        let text = String::from_utf8_lossy(&self.tail).into_owned();
        self.tail = rest;
        text
    }
    fn finish(&mut self) -> String {
        String::from_utf8_lossy(&std::mem::take(&mut self.tail)).into_owned()
    }
}

#[cfg(unix)]
fn signal_group(pid: Option<u32>, signal: i32) {
    if let Some(pid) = pid {
        unsafe {
            libc::kill(-(pid as i32), signal);
        }
    }
}
#[cfg(not(unix))]
fn signal_group(_pid: Option<u32>, _signal: i32) {}

async fn run_job(
    registry: ProcessRegistry,
    process: Arc<Process>,
    spec: JobSpec,
    mut controls: mpsc::UnboundedReceiver<Control>,
    hooks: JobHooks,
) {
    let JobSpec {
        command,
        cwd,
        timeout,
        stdin,
        ..
    } = spec;
    let JobHooks {
        parent_cancel,
        output,
        process: process_sink,
    } = hooks;
    let fail = |error: String| {
        finish_job(
            &registry,
            &process,
            ProcessState::Failed,
            None,
            None,
            Some(error),
            process_sink.clone(),
        )
    };
    let terminal = process.snapshot.lock().unwrap().background;
    // Output goes to one pipe, or for a background job to a pseudo-terminal
    // so programs show colors and progress as in a terminal.
    let (out_fd, err_fd, master) = match open_output(terminal) {
        Ok(fds) => fds,
        Err(error) => return fail(error.to_string()).await,
    };
    let mut cmd = tokio::process::Command::new("bash");
    cmd.arg("-c")
        .arg(&command)
        .current_dir(cwd)
        .stdin(if matches!(stdin, Stdin::Null) {
            Stdio::null()
        } else {
            Stdio::piped()
        })
        .stdout(out_fd)
        .stderr(err_fd)
        .process_group(0)
        .kill_on_drop(true);
    if terminal {
        // Nothing may wait on a pager; there is no terminal to type into.
        cmd.env("TERM", "xterm-256color")
            .env("PAGER", "cat")
            .env("GIT_PAGER", "cat");
    }
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(error) => return fail(error.to_string()).await,
    };
    let pid = child.id();
    process.snapshot.lock().unwrap().pid = pid;
    let version = registry.bump_version();
    if let Some(sink) = &process_sink {
        sink(registry.view_of(&process), version);
    }
    // Our copies of the write ends go, so the reader sees the end.
    drop(cmd);
    let mut rx = match master {
        Master::Pipe(reader) => match pipe::Receiver::from_owned_fd(reader) {
            Ok(rx) => Output::Pipe(rx),
            Err(error) => {
                signal_group(pid, libc::SIGKILL);
                return fail(error.to_string()).await;
            }
        },
        Master::Pty(master) => {
            let reader = match master.try_clone() {
                Ok(fd) => fd,
                Err(error) => {
                    signal_group(pid, libc::SIGKILL);
                    return fail(error.to_string()).await;
                }
            };
            *process.pty.lock().unwrap() = Some(master);
            Output::Pty(read_pty(reader))
        }
    };
    let mut input = child.stdin.take();
    if let Stdin::Text(text) = stdin
        && let Some(mut pipe) = input.take()
    {
        let _ = pipe.write_all(text.as_bytes()).await;
    }

    let mut out_decoder = Decoder::default();
    let mut cleaner = terminal.then(Cleaner::default);
    let mut status = None;
    let mut state = ProcessState::Running;
    let mut error = None;
    let mut kill_at = None;
    let deadline = timeout.map(|t| Instant::now() + t);
    let far = || Instant::now() + Duration::from_secs(86_400);
    let mut buf = vec![0u8; 16 * 1024];
    let mut pipe_open = true;
    loop {
        tokio::select! {
            chunk = rx.next(&mut buf), if pipe_open => match chunk { None => pipe_open = false, Some(bytes) => { let text = out_decoder.decode(&bytes); record_output(&registry, &process, &text, bytes.len(), &mut cleaner, &output, &process_sink); } },
            st = child.wait(), if status.is_none() => match st { Ok(st) => { status = Some(st); if kill_at.is_none() { signal_group(pid, libc::SIGTERM); kill_at = Some(Instant::now() + EXIT_GRACE); } }, Err(e) => { error = Some(e.to_string()); break; } },
            Some(control) = controls.recv(), if kill_at.is_none() => match control { Control::Write(data) => if let Some(pipe) = &mut input { let _ = pipe.write_all(&data).await; }, Control::CloseStdin => input = None, Control::Cancel => { state = ProcessState::Cancelled; signal_group(pid, libc::SIGTERM); kill_at = Some(Instant::now() + KILL_GRACE); } },
            _ = parent_cancel.cancelled(), if kill_at.is_none() => { state = ProcessState::Cancelled; signal_group(pid, libc::SIGTERM); kill_at = Some(Instant::now() + KILL_GRACE); }
            _ = sleep_until(deadline.unwrap_or_else(far)), if deadline.is_some() && kill_at.is_none() => { state = ProcessState::TimedOut; signal_group(pid, libc::SIGTERM); kill_at = Some(Instant::now() + KILL_GRACE); }
            _ = sleep_until(kill_at.unwrap_or_else(far)), if kill_at.is_some() => { signal_group(pid, libc::SIGKILL); break; }
            else => break,
        }
    }
    if status.is_none()
        && let Ok(Ok(st)) = tokio::time::timeout(Duration::from_secs(1), child.wait()).await
    {
        status = Some(st);
    }
    drop(input);
    signal_group(pid, libc::SIGKILL);
    process.pty.lock().unwrap().take();
    let text = out_decoder.finish();
    if !text.is_empty() {
        record_output(
            &registry,
            &process,
            &text,
            text.len(),
            &mut cleaner,
            &output,
            &process_sink,
        );
    }
    let (code, signal) = status.map_or((None, None), |st| (st.code(), st.signal()));
    if state == ProcessState::Running {
        state = ProcessState::Exited;
    }
    finish_job(
        &registry,
        &process,
        state,
        code,
        signal,
        error,
        process_sink,
    )
    .await;
}

fn record_output(
    registry: &ProcessRegistry,
    process: &Process,
    text: &str,
    bytes: usize,
    cleaner: &mut Option<Cleaner>,
    output: &Option<OutputSink>,
    process_sink: &Option<ProcessUpdateSink>,
) {
    if text.is_empty() {
        return;
    }
    let mut snapshot = process.snapshot.lock().unwrap();
    snapshot.output_bytes += bytes as u64;
    match cleaner {
        Some(c) => {
            c.apply(text, &mut snapshot.combined);
            snapshot.truncated |= append_bounded(&mut snapshot.combined, "");
        }
        None => snapshot.truncated |= append_bounded(&mut snapshot.combined, text),
    }
    let offset = snapshot.raw_total;
    snapshot.raw.push_str(text);
    snapshot.raw_total += text.len() as u64;
    if snapshot.raw.len() > MAX_RAW {
        let mut cut = snapshot.raw.len() - MAX_RAW;
        while !snapshot.raw.is_char_boundary(cut) {
            cut += 1;
        }
        snapshot.raw.drain(..cut);
        snapshot.raw_start += cut as u64;
    }
    drop(snapshot);
    let version = registry.bump_version();
    if let Some(sink) = process_sink {
        let mut view = registry.view_of(process);
        view.chunk = Some((offset, text.to_owned()));
        sink(view, version);
    }
    if let Some(output) = output {
        output(text);
    }
}

/// The last non-empty line, cut to `TAIL_MAX` bytes.
fn tail_of(text: &str) -> String {
    let line = text
        .rsplit('\n')
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_default();
    let mut end = line.len().min(TAIL_MAX);
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    line[..end].to_owned()
}

/// Where a job's output goes: a pipe's read end, or a pseudo-terminal's
/// master side.
enum Master {
    Pipe(OwnedFd),
    Pty(OwnedFd),
}

/// The child's stdout and stderr, and our side.
fn open_output(terminal: bool) -> std::io::Result<(Stdio, Stdio, Master)> {
    if !terminal {
        let (reader, writer) = std::io::pipe()?;
        let writer2 = writer.try_clone()?;
        return Ok((writer2.into(), writer.into(), Master::Pipe(reader.into())));
    }
    let (mut master, mut slave) = (-1, -1);
    let ws = libc::winsize {
        ws_row: PTY_ROWS,
        ws_col: PTY_COLS,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let r = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            &ws,
        )
    };
    if r != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    // The child gets only the slave side.
    unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
    let slave2 = slave.try_clone()?;
    Ok((slave2.into(), slave.into(), Master::Pty(master)))
}

/// Read a pseudo-terminal's master side on a thread of its own (it is not a
/// pipe tokio can poll). The channel ends when every writer is gone.
fn read_pty(master: OwnedFd) -> mpsc::UnboundedReceiver<Vec<u8>> {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        let mut file = std::fs::File::from(master);
        let mut buf = [0u8; 16 * 1024];
        loop {
            match file.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                // EIO: the last writer closed.
                Err(_) => break,
            }
        }
    });
    rx
}

enum Output {
    Pipe(pipe::Receiver),
    Pty(mpsc::UnboundedReceiver<Vec<u8>>),
}

impl Output {
    /// The next bytes, or None at the end.
    async fn next(&mut self, buf: &mut [u8]) -> Option<Vec<u8>> {
        match self {
            Output::Pipe(rx) => match rx.read(buf).await {
                Ok(0) | Err(_) => None,
                Ok(n) => Some(buf[..n].to_vec()),
            },
            Output::Pty(rx) => rx.recv().await,
        }
    }
}

/// Terminal output as text: escape sequences dropped, `\r\n` a newline, a
/// lone `\r` starting its line again (so a progress bar leaves its last
/// state), backspace taking a character back.
#[derive(Default)]
pub(super) struct Cleaner {
    esc: Esc,
    cr: bool,
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Esc {
    #[default]
    None,
    Start,
    Csi,
    Osc,
    /// ESC inside an OSC: the start of its `ESC \\` ending.
    OscEnd,
}

impl Cleaner {
    pub(super) fn apply(&mut self, text: &str, out: &mut String) {
        for c in text.chars() {
            match self.esc {
                Esc::None => {}
                Esc::Start => {
                    self.esc = match c {
                        '[' => Esc::Csi,
                        ']' => Esc::Osc,
                        _ => Esc::None,
                    };
                    continue;
                }
                Esc::Csi => {
                    if ('\x40'..='\x7e').contains(&c) {
                        self.esc = Esc::None;
                    }
                    continue;
                }
                Esc::Osc => {
                    match c {
                        '\x07' => self.esc = Esc::None,
                        '\x1b' => self.esc = Esc::OscEnd,
                        _ => {}
                    }
                    continue;
                }
                Esc::OscEnd => {
                    self.esc = Esc::None;
                    continue;
                }
            }
            if std::mem::take(&mut self.cr) && c != '\n' {
                let start = out.rfind('\n').map_or(0, |i| i + 1);
                out.truncate(start);
            }
            match c {
                '\x1b' => self.esc = Esc::Start,
                '\r' => self.cr = true,
                '\x08' => {
                    if !out.ends_with('\n') {
                        out.pop();
                    }
                }
                '\n' | '\t' => out.push(c),
                c if c.is_control() => {}
                c => out.push(c),
            }
        }
    }
}

fn append_bounded(target: &mut String, text: &str) -> bool {
    target.push_str(text);
    if target.len() <= MAX_OUTPUT {
        return false;
    }
    let mut start = target.len() - MAX_OUTPUT;
    while !target.is_char_boundary(start) {
        start += 1;
    }
    target.drain(..start);
    true
}

async fn finish_job(
    registry: &ProcessRegistry,
    process: &Process,
    state: ProcessState,
    code: Option<i32>,
    signal: Option<i32>,
    error: Option<String>,
    process_sink: Option<ProcessUpdateSink>,
) {
    {
        let mut snapshot = process.snapshot.lock().unwrap();
        snapshot.state = state;
        snapshot.code = code;
        snapshot.signal = signal;
        snapshot.error = error;
        snapshot.finished = Some(Instant::now());
        snapshot.finished_at_ms = Some(unix_ms());
    }
    process.done.notify_waiters();
    registry.prune();
    let version = registry.bump_version();
    if let Some(sink) = process_sink {
        sink(registry.view_of(process), version);
    }
    // The process event is the completion signal for detached commands. A
    // background start already completed its tool call when it returned.
}

fn json_snapshot(snapshot: &Snapshot) -> Value {
    let output = truncate_middle(snapshot.combined.trim_end(), SHOW_HEAD, SHOW_TAIL);
    json!({ "id": snapshot.id, "state": snapshot.state.name(), "running": snapshot.state == ProcessState::Running, "command": snapshot.command, "pid": snapshot.pid, "started_at_ms": snapshot.started_at_ms, "finished_at_ms": snapshot.finished_at_ms, "elapsed_ms": snapshot.finished.map_or_else(|| snapshot.started.elapsed().as_millis() as u64, |end| end.duration_since(snapshot.started).as_millis() as u64), "output": output, "output_bytes": snapshot.output_bytes, "truncated": snapshot.truncated || output.len() < snapshot.combined.trim_end().len(), "code": snapshot.code, "signal": snapshot.signal, "error": snapshot.error })
}

fn render(snapshot: &Snapshot) -> String {
    let mut result = if snapshot.combined.trim().is_empty() {
        "(no output)".into()
    } else {
        truncate_middle(snapshot.combined.trim_end(), SHOW_HEAD, SHOW_TAIL)
    };
    match snapshot.state {
        ProcessState::Exited => match (snapshot.code, snapshot.signal) {
            (Some(code), _) => result.push_str(&format!("\n[exit code: {code}]")),
            (None, Some(signal)) => result.push_str(&format!("\n[killed by signal {signal}]")),
            _ => {}
        },
        ProcessState::Cancelled => result.push_str("\n[cancelled; process killed]"),
        ProcessState::TimedOut => result.push_str(&format!(
            "\n[timed out after {}s; process killed]",
            snapshot.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS)
        )),
        ProcessState::Failed => result.push_str(&format!(
            "\n[failed: {}]",
            snapshot.error.as_deref().unwrap_or("unknown error")
        )),
        ProcessState::Running => {}
    }
    result
}

impl Tool for Shell {
    fn spec(&self) -> &ToolSpec {
        &self.0
    }
    fn call<'a>(&'a self, args: Value, ctx: &'a ToolContext) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            let args: Args = typed_args(args)?;
            match args.action.as_str() {
                "run" | "start" => run_action(args, ctx).await,
                "list" => Ok(serde_json::to_string_pretty(
                    &ctx.jobs
                        .list(&ctx.session_id)
                        .iter()
                        .map(json_snapshot)
                        .collect::<Vec<_>>(),
                )
                .unwrap()),
                "status" | "read" => {
                    let id = args
                        .id
                        .as_deref()
                        .ok_or("id is required for this action".to_owned())?;
                    Ok(serde_json::to_string_pretty(&json_snapshot(
                        &ctx.jobs.snapshot(&ctx.session_id, id)?,
                    ))
                    .unwrap())
                }
                "wait" => {
                    let id = args
                        .id
                        .as_deref()
                        .ok_or("id is required for wait".to_owned())?;
                    let timeout = Duration::from_secs(
                        args.timeout_secs
                            .unwrap_or(DEFAULT_TIMEOUT_SECS)
                            .clamp(1, MAX_TIMEOUT_SECS),
                    );
                    let snapshot = ctx.jobs.wait(&ctx.session_id, id, Some(timeout)).await?;
                    if snapshot.state == ProcessState::Exited {
                        Ok(render(&snapshot))
                    } else {
                        Err(render(&snapshot))
                    }
                }
                "kill" => {
                    let id = args
                        .id
                        .as_deref()
                        .ok_or("id is required for kill".to_owned())?;
                    ctx.jobs.cancel(&ctx.session_id, id)?;
                    Ok(format!("cancel requested: {id}"))
                }
                "write" => {
                    let id = args
                        .id
                        .as_deref()
                        .ok_or("id is required for write".to_owned())?;
                    ctx.jobs.write(
                        &ctx.session_id,
                        id,
                        args.data.unwrap_or_default().into_bytes(),
                    )?;
                    Ok(format!("wrote to {id}"))
                }
                "close_stdin" => {
                    let id = args
                        .id
                        .as_deref()
                        .ok_or("id is required for close_stdin".to_owned())?;
                    ctx.jobs.close_stdin(&ctx.session_id, id)?;
                    Ok(format!("closed stdin: {id}"))
                }
                other => Err(format!("unknown shell action {other:?}")),
            }
        })
    }
}

async fn run_action(args: Args, ctx: &ToolContext) -> ToolResult {
    let command = args
        .command
        .ok_or("command is required for run/start".to_owned())?;
    let start = args.action == "start" || args.mode.as_deref() == Some("start") || args.background;
    let timeout = args
        .timeout_secs
        .map(|s| Duration::from_secs(s.clamp(1, MAX_TIMEOUT_SECS)))
        .or_else(|| (!start).then_some(Duration::from_secs(DEFAULT_TIMEOUT_SECS)));
    let stdin = match args.stdin {
        Some(text) if text == "pipe" => Stdin::Pipe,
        Some(text) => Stdin::Text(text),
        None => Stdin::Null,
    };
    let id = ctx.jobs.spawn(
        JobSpec {
            owner: ctx.session_id.clone(),
            command,
            cwd: ctx.cwd.clone(),
            timeout,
            stdin,
            background: start,
        },
        JobHooks {
            // A detached start owns its lifetime; cancelling the model turn
            // must not kill a background process the model explicitly kept.
            parent_cancel: if start {
                CancellationToken::new()
            } else {
                ctx.cancel.clone()
            },
            output: if start { None } else { ctx.output.clone() },
            process: if start { ctx.processes.clone() } else { None },
        },
    );
    if start {
        return Ok(format!("background process started: {id}"));
    }
    let snapshot = ctx.jobs.wait(&ctx.session_id, &id, None).await?;
    if snapshot.state == ProcessState::Exited {
        Ok(render(&snapshot))
    } else {
        Err(render(&snapshot))
    }
}

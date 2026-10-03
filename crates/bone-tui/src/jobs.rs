//! Streaming jobs for TUI Lua (`bone.job`): a process with a handle. Output
//! reaches Lua as it arrives, the job can be written to, cancelled or timed
//! out, and its status can be read at any time. The UI never waits on it.
//!
//! Each job runs in its own task, in its own process group, so cancelling
//! (SIGTERM, then SIGKILL after a grace period) also stops whatever it
//! started. The task reports back through the app's event channel in order:
//! started, output chunks, exited.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use serde_json::{Value as Json, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::app::{App, AppEvent};

/// After SIGTERM, how long before SIGKILL.
const KILL_GRACE: Duration = Duration::from_secs(2);
/// Output kept for `on_exit` per stream; beyond this the start is dropped.
const BUFFER_MAX: usize = 4 << 20;
/// Finished jobs remembered for `status`/`list`.
const FINISHED_KEPT: usize = 32;

/// What to run: `bash -c cmd`, or an argv list.
#[derive(Debug, Clone)]
pub enum Command {
    Shell(String),
    Argv(Vec<String>),
}

impl Command {
    fn display(&self) -> String {
        match self {
            Command::Shell(s) => s.clone(),
            Command::Argv(v) => v.join(" "),
        }
    }
}

/// What to give the process on stdin.
#[derive(Debug, Clone)]
pub enum Stdin {
    Null,
    /// Write this, then close.
    Text(String),
    /// Keep it open for `job:write`.
    Open,
}

#[derive(Debug, Clone)]
pub struct Spec {
    pub command: Command,
    pub cwd: Option<String>,
    pub env: Vec<(String, String)>,
    pub stdin: Stdin,
    pub timeout: Option<Duration>,
}

/// Requests from the UI to a running job.
enum Ctl {
    Write(Vec<u8>),
    CloseStdin,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Running,
    Exited,
    Cancelled,
    TimedOut,
    Failed,
}

impl State {
    fn name(self) -> &'static str {
        match self {
            State::Running => "running",
            State::Exited => "exited",
            State::Cancelled => "cancelled",
            State::TimedOut => "timed_out",
            State::Failed => "failed",
        }
    }
}

/// How a job ended, from its task.
#[derive(Debug, Default)]
struct Exit {
    code: Option<i32>,
    signal: Option<i32>,
    cancelled: bool,
    timed_out: bool,
    /// It could not start or could not be waited on.
    error: Option<String>,
}

/// Output of one stream as the UI sees it.
#[derive(Default)]
struct Output {
    /// Bytes of an incomplete UTF-8 character at the end of the last chunk.
    tail: Vec<u8>,
    /// The incomplete last line, in line mode.
    line: String,
    /// Everything so far, for `on_exit` (when buffering).
    kept: String,
    truncated: bool,
    bytes: u64,
}

impl Output {
    /// Decode a chunk, holding back a split character.
    fn decode(&mut self, chunk: &[u8]) -> String {
        self.bytes += chunk.len() as u64;
        self.tail.extend_from_slice(chunk);
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

    fn keep(&mut self, text: &str) {
        self.kept.push_str(text);
        if self.kept.len() > BUFFER_MAX {
            let mut cut = self.kept.len() - BUFFER_MAX;
            while !self.kept.is_char_boundary(cut) {
                cut += 1;
            }
            self.kept.drain(..cut);
            self.truncated = true;
        }
    }
}

pub struct Job {
    pub id: u64,
    pub name: Option<String>,
    command: String,
    pid: Option<u32>,
    state: State,
    started: Instant,
    finished: Option<Instant>,
    exit: Option<Exit>,
    ctl: mpsc::UnboundedSender<Ctl>,
    /// Deliver whole lines instead of chunks.
    lines: bool,
    /// Keep output for `on_exit`.
    buffer: bool,
    out: Output,
    err: Output,
    on_stdout: Option<u64>,
    on_stderr: Option<u64>,
    on_exit: Option<u64>,
}

impl Job {
    pub fn running(&self) -> bool {
        self.state == State::Running
    }

    fn callbacks(&self) -> impl Iterator<Item = u64> {
        [self.on_stdout, self.on_stderr, self.on_exit]
            .into_iter()
            .flatten()
    }

    /// The job as Lua sees it (`job:status()`, `bone.job.list()`).
    pub fn status(&self) -> Json {
        let end = self.finished.unwrap_or_else(Instant::now);
        let mut v = json!({
            "id": self.id,
            "name": self.name,
            "cmd": self.command,
            "pid": self.pid,
            "state": self.state.name(),
            "running": self.running(),
            "elapsed_ms": end.duration_since(self.started).as_millis() as u64,
            "stdout_bytes": self.out.bytes,
            "stderr_bytes": self.err.bytes,
        });
        if let Some(e) = &self.exit {
            v["code"] = json!(e.code);
            v["signal"] = json!(e.signal);
            v["error"] = json!(e.error);
        }
        v
    }

    /// What `on_exit` gets: the status plus the kept output.
    fn result(&self) -> Json {
        let mut v = self.status();
        let e = self.exit.as_ref();
        v["cancelled"] = json!(e.is_some_and(|e| e.cancelled));
        v["timed_out"] = json!(e.is_some_and(|e| e.timed_out));
        v["duration_ms"] = v["elapsed_ms"].clone();
        if self.buffer {
            v["stdout"] = json!(self.out.kept);
            v["stderr"] = json!(self.err.kept);
            v["truncated"] = json!(self.out.truncated || self.err.truncated);
        }
        v
    }
}

/// The jobs of an app: running ones and the last few finished ones.
#[derive(Default)]
pub struct Jobs {
    pub list: Vec<Job>,
    finished: VecDeque<u64>,
}

impl Jobs {
    pub fn get(&self, id: u64) -> Option<&Job> {
        self.list.iter().find(|j| j.id == id)
    }

    fn get_mut(&mut self, id: u64) -> Option<&mut Job> {
        self.list.iter_mut().find(|j| j.id == id)
    }

    /// Drop a job's callbacks (its plugin unloaded); returns them to release.
    pub fn forget_callbacks(&mut self, id: u64) -> Vec<u64> {
        let Some(j) = self.get_mut(id) else {
            return Vec::new();
        };
        let cbs = j.callbacks().collect();
        j.on_stdout = None;
        j.on_stderr = None;
        j.on_exit = None;
        cbs
    }

    pub fn running(&self) -> usize {
        self.list.iter().filter(|j| j.running()).count()
    }

    /// Remember a finished job, forgetting the oldest beyond the limit.
    /// Returns the forgotten ids.
    fn retire(&mut self, id: u64) -> Vec<u64> {
        self.finished.push_back(id);
        let mut gone = Vec::new();
        while self.finished.len() > FINISHED_KEPT {
            if let Some(old) = self.finished.pop_front() {
                self.list.retain(|j| j.id != old);
                gone.push(old);
            }
        }
        gone
    }
}

/// Options `bone.job.start` takes besides the process itself.
pub struct Callbacks {
    pub name: Option<String>,
    pub lines: bool,
    pub buffer: bool,
    pub on_stdout: Option<u64>,
    pub on_stderr: Option<u64>,
    pub on_exit: Option<u64>,
}

impl App {
    /// Start a job; its callbacks run on the UI thread as it goes.
    pub fn start_job(&mut self, spec: Spec, cb: Callbacks) -> u64 {
        self.next_callback += 1;
        let id = self.next_callback;
        let (ctl, ctl_rx) = mpsc::unbounded_channel();
        let command = spec.command.display();
        self.jobs.list.push(Job {
            id,
            name: cb.name,
            command: command.clone(),
            pid: None,
            state: State::Running,
            started: Instant::now(),
            finished: None,
            exit: None,
            ctl,
            lines: cb.lines,
            buffer: cb.buffer,
            out: Output::default(),
            err: Output::default(),
            on_stdout: cb.on_stdout,
            on_stderr: cb.on_stderr,
            on_exit: cb.on_exit,
        });
        let tx = self.event_sender();
        tokio::spawn(run(id, spec, ctl_rx, tx));
        self.dirty = true;
        id
    }

    /// Ask a running job to stop. Returns whether it was running.
    pub fn cancel_job(&mut self, id: u64) -> bool {
        match self.jobs.get(id) {
            Some(j) if j.running() => j.ctl.send(Ctl::Cancel).is_ok(),
            _ => false,
        }
    }

    /// Write to a job's stdin (opened with `stdin = true`).
    pub fn write_job(&mut self, id: u64, data: Vec<u8>) -> bool {
        match self.jobs.get(id) {
            Some(j) if j.running() => j.ctl.send(Ctl::Write(data)).is_ok(),
            _ => false,
        }
    }

    pub fn close_job_stdin(&mut self, id: u64) -> bool {
        match self.jobs.get(id) {
            Some(j) if j.running() => j.ctl.send(Ctl::CloseStdin).is_ok(),
            _ => false,
        }
    }

    fn job_started(&mut self, id: u64, pid: Option<u32>) {
        let Some(j) = self.jobs.get_mut(id) else {
            return;
        };
        j.pid = pid;
        let ev = json!({ "id": id, "name": j.name, "cmd": j.command, "pid": pid });
        self.fire("job/started", ev);
    }

    fn job_output(&mut self, id: u64, stream: Stream, chunk: &[u8]) {
        let Some(j) = self.jobs.get_mut(id) else {
            return;
        };
        let (out, cb) = match stream {
            Stream::Stdout => (&mut j.out, j.on_stdout),
            Stream::Stderr => (&mut j.err, j.on_stderr),
        };
        let text = out.decode(chunk);
        if j.buffer {
            out.keep(&text);
        }
        let pieces = if j.lines {
            out.line.push_str(&text);
            split_lines(&mut out.line)
        } else if text.is_empty() {
            Vec::new()
        } else {
            vec![text]
        };
        if let Some(cb) = cb {
            for piece in pieces {
                self.job_callback(cb, id, "job output", json!(piece));
            }
        }
    }

    fn job_exited(&mut self, id: u64, exit: Exit) {
        let Some(j) = self.jobs.get_mut(id) else {
            return;
        };
        // Whatever is left: a split character, a last line without "\n".
        let mut rest = Vec::new();
        for (out, cb) in [(&mut j.out, j.on_stdout), (&mut j.err, j.on_stderr)] {
            let text = String::from_utf8_lossy(&std::mem::take(&mut out.tail)).into_owned();
            if j.buffer {
                out.keep(&text);
            }
            let text = if j.lines {
                out.line.push_str(&text);
                std::mem::take(&mut out.line)
            } else {
                text
            };
            if let (Some(cb), false) = (cb, text.is_empty()) {
                rest.push((cb, text));
            }
        }
        for (cb, text) in rest {
            self.job_callback(cb, id, "job output", json!(text));
        }
        let Some(j) = self.jobs.get_mut(id) else {
            return;
        };
        j.state = if exit.error.is_some() {
            State::Failed
        } else if exit.timed_out {
            State::TimedOut
        } else if exit.cancelled {
            State::Cancelled
        } else {
            State::Exited
        };
        j.finished = Some(Instant::now());
        j.exit = Some(exit);
        let result = j.result();
        if let Some(cb) = j.on_exit {
            self.job_callback(cb, id, "job on_exit", result.clone());
        }
        let callbacks: Vec<u64> = self
            .jobs
            .get(id)
            .map(|j| j.callbacks().collect())
            .unwrap_or_default();
        self.release_callbacks(&callbacks);
        if let Some(j) = self.jobs.get_mut(id) {
            j.on_stdout = None;
            j.on_stderr = None;
            j.on_exit = None;
        }
        self.jobs.retire(id);
        self.fire("job/finished", result);
        self.dirty = true;
    }

    /// Call a job callback with `(data, job)`.
    fn job_callback(&mut self, cb: u64, id: u64, context: &str, data: Json) {
        self.call_callback_multi(cb, context, |lua| {
            Ok(vec![
                bone_lua::to_lua(lua, &data)?,
                crate::lua::job_handle(lua, id)?,
            ]
            .into())
        });
    }
}

/// Take the complete lines (without their "\n" or "\r\n") out of `buf`.
fn split_lines(buf: &mut String) -> Vec<String> {
    let Some(end) = buf.rfind('\n') else {
        return Vec::new();
    };
    let rest = buf.split_off(end + 1);
    let lines = buf
        .lines()
        .map(|l| l.strip_suffix('\r').unwrap_or(l).to_owned())
        .collect();
    *buf = rest;
    lines
}

/// Send work to the UI thread.
fn send(tx: &mpsc::UnboundedSender<AppEvent>, f: impl FnOnce(&mut App) + Send + 'static) {
    let _ = tx.send(AppEvent(Box::new(f)));
}

/// Stops the process group if the job's task ends early (the TUI quits).
struct Group(Option<u32>);

impl Group {
    fn signal(&self, sig: i32) {
        #[cfg(unix)]
        if let Some(pid) = self.0 {
            // SAFETY: kill(2) on our own child's process group.
            unsafe {
                libc::kill(-(pid as i32), sig);
            }
        }
        #[cfg(not(unix))]
        let _ = sig;
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        #[cfg(unix)]
        self.signal(libc::SIGKILL);
    }
}

#[cfg(unix)]
const SIGTERM: i32 = libc::SIGTERM;
#[cfg(unix)]
const SIGKILL: i32 = libc::SIGKILL;
#[cfg(not(unix))]
const SIGTERM: i32 = 15;
#[cfg(not(unix))]
const SIGKILL: i32 = 9;

async fn pump(
    mut r: impl AsyncRead + Unpin,
    id: u64,
    stream: Stream,
    tx: mpsc::UnboundedSender<AppEvent>,
) {
    let mut buf = vec![0u8; 8192];
    loop {
        match r.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                let chunk = buf[..n].to_vec();
                send(&tx, move |app| app.job_output(id, stream, &chunk));
            }
        }
    }
}

/// The job's task: spawn, pump output, obey the UI, report the end.
async fn run(
    id: u64,
    spec: Spec,
    mut ctl: mpsc::UnboundedReceiver<Ctl>,
    tx: mpsc::UnboundedSender<AppEvent>,
) {
    let mut cmd = match &spec.command {
        Command::Shell(s) => {
            let mut c = tokio::process::Command::new("bash");
            c.arg("-c").arg(s);
            c
        }
        Command::Argv(argv) => {
            let mut c = tokio::process::Command::new(&argv[0]);
            c.args(&argv[1..]);
            c
        }
    };
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(match spec.stdin {
            Stdin::Null => std::process::Stdio::null(),
            _ => std::process::Stdio::piped(),
        })
        .envs(spec.env.iter().cloned())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    if let Some(cwd) = &spec.cwd {
        cmd.current_dir(cwd);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let error = format!("cannot run {}: {e}", spec.command.display());
            send(&tx, move |app| {
                app.job_exited(
                    id,
                    Exit {
                        error: Some(error),
                        ..Default::default()
                    },
                )
            });
            return;
        }
    };
    let mut group = Group(child.id());
    let pid = child.id();
    send(&tx, move |app| app.job_started(id, pid));
    let readers = [
        child
            .stdout
            .take()
            .map(|r| tokio::spawn(pump(r, id, Stream::Stdout, tx.clone()))),
        child
            .stderr
            .take()
            .map(|r| tokio::spawn(pump(r, id, Stream::Stderr, tx.clone()))),
    ];
    let mut stdin = child.stdin.take();
    if let Stdin::Text(text) = &spec.stdin
        && let Some(mut pipe) = stdin.take()
    {
        let text = text.clone();
        tokio::spawn(async move {
            let _ = pipe.write_all(text.as_bytes()).await;
        });
    }

    let mut exit = Exit::default();
    let deadline = spec.timeout.map(|t| tokio::time::Instant::now() + t);
    let mut kill_at: Option<tokio::time::Instant> = None;
    let far = || tokio::time::Instant::now() + Duration::from_secs(86_400);
    let status = loop {
        tokio::select! {
            st = child.wait() => break st,
            Some(c) = ctl.recv() => match c {
                Ctl::Write(data) => {
                    if let Some(pipe) = &mut stdin {
                        let _ = pipe.write_all(&data).await;
                    }
                }
                Ctl::CloseStdin => stdin = None,
                Ctl::Cancel if kill_at.is_none() => {
                    exit.cancelled = true;
                    group.signal(SIGTERM);
                    kill_at = Some(tokio::time::Instant::now() + KILL_GRACE);
                }
                Ctl::Cancel => {}
            },
            _ = tokio::time::sleep_until(deadline.unwrap_or_else(far)), if deadline.is_some() && kill_at.is_none() => {
                exit.timed_out = true;
                group.signal(SIGTERM);
                kill_at = Some(tokio::time::Instant::now() + KILL_GRACE);
            }
            _ = tokio::time::sleep_until(kill_at.unwrap_or_else(far)), if kill_at.is_some() => {
                group.signal(SIGKILL);
                kill_at = Some(far());
            }
        }
    };
    drop(stdin);
    match status {
        Ok(st) => {
            exit.code = st.code();
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                exit.signal = st.signal();
            }
        }
        Err(e) => exit.error = Some(e.to_string()),
    }
    // Anything still holding the pipes goes with the group.
    if exit.cancelled || exit.timed_out {
        group.signal(SIGKILL);
    }
    for r in readers.into_iter().flatten() {
        let _ = tokio::time::timeout(Duration::from_secs(1), r).await;
    }
    // Only an interrupted task (the TUI quitting) kills on drop: once the
    // leader is reaped its id may be reused.
    group.0 = None;
    send(&tx, move |app| app.job_exited(id, exit));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoding_holds_back_split_characters() {
        let mut o = Output::default();
        let bytes = "é!".as_bytes();
        assert_eq!(o.decode(&bytes[..1]), "");
        assert_eq!(o.decode(&bytes[1..]), "é!");
        assert_eq!(o.bytes, 3);
        assert_eq!(o.decode(b"\xff"), "\u{fffd}");
    }

    #[test]
    fn lines_split_off_complete_ones() {
        let mut buf = "a\r\nb\nc".to_owned();
        assert_eq!(split_lines(&mut buf), ["a", "b"]);
        assert_eq!(buf, "c");
        assert!(split_lines(&mut buf).is_empty());
    }

    #[test]
    fn kept_output_drops_the_start() {
        let mut o = Output::default();
        o.keep(&"x".repeat(BUFFER_MAX));
        o.keep("yz");
        assert_eq!(o.kept.len(), BUFFER_MAX);
        assert!(o.truncated && o.kept.ends_with("yz"));
    }
}

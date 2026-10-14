//! Streaming jobs for TUI Lua (`bone.job`): a process with a handle. Output
//! reaches Lua as it arrives, the job can be written to, cancelled or timed
//! out, and its status can be read at any time. The UI never waits on it.
//!
//! Each job runs in its own task, in its own process group, so cancelling
//! (SIGTERM or SIGHUP, then SIGKILL after a grace period) also stops whatever it
//! started. The task reports back through the app's event channel in order:
//! started, output chunks, exited.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use serde_json::{Value as Json, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::app::{App, AppEvent};
use crate::term::Term;

/// After SIGTERM, how long before SIGKILL.
const KILL_GRACE: Duration = Duration::from_secs(2);
/// Output kept for `on_exit` per stream; beyond this the start is dropped.
const BUFFER_MAX: usize = 4 << 20;
/// Finished jobs remembered for `status`/`list`.
const FINISHED_KEPT: usize = 32;

/// What to run: a shell command (`bash -c`, PowerShell on Windows), or an
/// argv list.
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
    /// Run in a pseudo-terminal of (rows, cols) (Unix): its output plays into
    /// a screen (`job:screen`) and `job:write` types into it.
    pub pty: Option<(u16, u16)>,
}

/// Requests from the UI to a running job.
enum Ctl {
    Write(Vec<u8>),
    CloseStdin,
    Cancel,
    Resize(u16, u16),
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
    /// Holds an incomplete UTF-8 character at the end of the last chunk.
    tail: bone_lua::sys::Utf8Decoder,
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
        self.tail.decode(chunk)
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
    /// The screen of a `pty` job.
    pub term: Option<Term>,
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
        let term = spec.pty.map(|(rows, cols)| Term::live(rows, cols));
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
            term,
        });
        let tx = self.event_sender();
        tokio::spawn(run(id, spec, ctl_rx, tx));
        self.dirty = true;
        id
    }

    /// Ask a running job to stop, and drop its screen. Returns whether it was
    /// running.
    pub fn cancel_job(&mut self, id: u64) -> bool {
        let Some(j) = self.jobs.get_mut(id) else {
            return false;
        };
        j.term = None;
        j.running() && j.ctl.send(Ctl::Cancel).is_ok()
    }

    /// Write to a job's stdin (opened with `stdin = true`).
    pub fn write_job(&mut self, id: u64, data: Vec<u8>) -> bool {
        match self.jobs.get(id) {
            Some(j) if j.running() => j.ctl.send(Ctl::Write(data)).is_ok(),
            _ => false,
        }
    }

    /// A `pty` job's screen at `rows` × `cols` (resizing its terminal to
    /// that first), scrolled back `scroll` rows: its lines, the scroll, how
    /// far it could scroll, and the state keys and paste depend on.
    pub fn job_screen(
        &mut self,
        id: u64,
        rows: u16,
        cols: u16,
        scroll: usize,
    ) -> Option<(Vec<Vec<crate::ui::Item>>, usize, usize, Json)> {
        let j = self.jobs.get_mut(id)?;
        let running = j.running();
        let term = j.term.as_mut()?;
        if running && term.state().map(|s| s.size()) != Some((rows, cols)) {
            let _ = j.ctl.send(Ctl::Resize(rows, cols));
        }
        let (lines, scroll, max) = term.lines(rows, cols, scroll, &mut self.theme);
        let s = term.state()?;
        let (row, col) = s.cursor_position();
        let state = json!({
            "cursor": (!s.hide_cursor() && scroll == 0).then_some([row + 1, col + 1]),
            "title": s.title(),
            "app_cursor": s.application_cursor(),
            "bracketed_paste": s.bracketed_paste(),
            "alt_screen": s.alternate_screen(),
            "running": running,
        });
        Some((lines, scroll, max, state))
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
        if let Some(term) = &mut j.term {
            let reply = term.feed(chunk);
            if !reply.is_empty() {
                let _ = j.ctl.send(Ctl::Write(reply));
            }
            self.dirty = true;
            if j.on_stdout.is_none() {
                j.out.bytes += chunk.len() as u64;
                return;
            }
        }
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
            let text = out.tail.finish();
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
/// A pty job's is hung up instead, as closing a terminal does: shells ignore
/// SIGTERM, and pass SIGHUP on to their jobs.
struct Group(Option<u32>, bool);

impl Group {
    fn terminate(&self) {
        if let Some(pid) = self.0 {
            bone_lua::sys::terminate_tree(pid, self.1);
        }
    }

    fn kill(&self) {
        if let Some(pid) = self.0 {
            bone_lua::sys::kill_tree(pid);
        }
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        self.kill();
    }
}

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

/// A pty job's output, input, and how to resize it.
type Pty = (
    Box<dyn AsyncRead + Unpin + Send>,
    Box<dyn AsyncWrite + Unpin + Send>,
    Box<dyn Fn(u16, u16) + Send>,
);

/// Give `cmd` a new pseudo-terminal as its stdin, stdout, stderr and
/// controlling terminal.
#[cfg(unix)]
fn attach_pty(cmd: &mut tokio::process::Command, rows: u16, cols: u16) -> std::io::Result<Pty> {
    use std::os::fd::AsRawFd;
    use tokio::net::unix::pipe;
    let (master, slave) = bone_lua::sys::open_pty(rows, cols)?;
    cmd.stdin(slave.try_clone()?)
        .stdout(slave.try_clone()?)
        .stderr(slave)
        .env("TERM", "xterm-256color");
    bone_lua::sys::controlling_tty(cmd.as_std_mut());
    // tokio reads and writes it without blocking a thread.
    let fd = master.as_raw_fd();
    // SAFETY: fcntl on our own descriptor.
    unsafe {
        libc::fcntl(
            fd,
            libc::F_SETFL,
            libc::fcntl(fd, libc::F_GETFL) | libc::O_NONBLOCK,
        )
    };
    let out = pipe::Receiver::from_owned_fd_unchecked(master.try_clone()?)?;
    let input = pipe::Sender::from_owned_fd_unchecked(master.try_clone()?)?;
    let resize = move |rows, cols| drop(bone_lua::sys::set_winsize(&master, rows, cols));
    Ok((Box::new(out), Box::new(input), Box::new(resize)))
}

#[cfg(not(unix))]
fn attach_pty(_: &mut tokio::process::Command, _: u16, _: u16) -> std::io::Result<Pty> {
    Err(std::io::Error::other("terminals need Unix for now"))
}

/// The job's task: spawn, pump output, obey the UI, report the end.
async fn run(
    id: u64,
    spec: Spec,
    mut ctl: mpsc::UnboundedReceiver<Ctl>,
    tx: mpsc::UnboundedSender<AppEvent>,
) {
    let mut cmd = match &spec.command {
        Command::Shell(s) => tokio::process::Command::from(bone_lua::sys::shell(s)),
        Command::Argv(argv) => {
            let mut c = tokio::process::Command::new(bone_lua::sys::program(&argv[0]));
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
    if let Some(cwd) = &spec.cwd {
        cmd.current_dir(cwd);
    }
    // A pty job leads a session of its own instead (also a new group).
    let pty = (spec.pty)
        .map(|(rows, cols)| attach_pty(&mut cmd, rows, cols))
        .transpose();
    if spec.pty.is_none() {
        bone_lua::sys::new_group(cmd.as_std_mut());
    }
    let spawned = pty.and_then(|pty| Ok((cmd.spawn()?, pty)));
    // Our copies of a pty's slave side go, so reading it ends with the job.
    drop(cmd);
    let (mut child, pty) = match spawned {
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
    let mut group = Group(child.id(), pty.is_some());
    let pid = child.id();
    send(&tx, move |app| app.job_started(id, pid));
    let (pty_out, stdin, resize) = match pty {
        Some((out, input, resize)) => (Some(out), Some(input), Some(resize)),
        None => (
            None,
            (child.stdin.take()).map(|s| Box::new(s) as Box<dyn AsyncWrite + Unpin + Send>),
            None,
        ),
    };
    // Writes have a task of their own: a program not reading never holds up
    // the job.
    let (mut stdin, writer) = stdin
        .map(|mut pipe| {
            let (w, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
            let task = async move {
                while let Some(data) = rx.recv().await {
                    if pipe.write_all(&data).await.is_err() {
                        return;
                    }
                }
            };
            (w, tokio::spawn(task))
        })
        .unzip();
    let tasks = [
        child
            .stdout
            .take()
            .map(|r| tokio::spawn(pump(r, id, Stream::Stdout, tx.clone()))),
        child
            .stderr
            .take()
            .map(|r| tokio::spawn(pump(r, id, Stream::Stderr, tx.clone()))),
        pty_out.map(|r| tokio::spawn(pump(r, id, Stream::Stdout, tx.clone()))),
        writer,
    ];
    if let Stdin::Text(text) = spec.stdin
        && let Some(w) = &stdin
    {
        let _ = w.send(text.into_bytes());
        // A terminal stays open for typing into.
        if resize.is_none() {
            stdin = None;
        }
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
                    if let Some(w) = &stdin {
                        let _ = w.send(data);
                    }
                }
                Ctl::Resize(rows, cols) => {
                    if let Some(resize) = &resize {
                        resize(rows, cols);
                    }
                }
                // A terminal's end of input is typing ctrl+d.
                Ctl::CloseStdin if resize.is_some() => {
                    let _ = stdin.as_ref().map(|w| w.send(vec![4]));
                }
                Ctl::CloseStdin => stdin = None,
                Ctl::Cancel if kill_at.is_none() => {
                    exit.cancelled = true;
                    group.terminate();
                    kill_at = Some(tokio::time::Instant::now() + KILL_GRACE);
                }
                Ctl::Cancel => {}
            },
            _ = tokio::time::sleep_until(deadline.unwrap_or_else(far)), if deadline.is_some() && kill_at.is_none() => {
                exit.timed_out = true;
                group.terminate();
                kill_at = Some(tokio::time::Instant::now() + KILL_GRACE);
            }
            _ = tokio::time::sleep_until(kill_at.unwrap_or_else(far)), if kill_at.is_some() => {
                group.kill();
                kill_at = Some(far());
            }
        }
    };
    drop(stdin);
    match status {
        Ok(st) => {
            exit.code = st.code();
            exit.signal = bone_lua::sys::exit_signal(&st);
        }
        Err(e) => exit.error = Some(e.to_string()),
    }
    // Anything still holding the pipes goes with the group.
    if exit.cancelled || exit.timed_out {
        group.kill();
    }
    // A pipe still held by something outside the group never ends.
    for mut r in tasks.into_iter().flatten() {
        let _ = tokio::time::timeout(Duration::from_secs(1), &mut r).await;
        r.abort();
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

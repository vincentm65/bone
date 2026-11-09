//! Terminal jobs on Windows: the program in a ConPTY (a pseudo console),
//! through portable-pty. Its reads, writes and wait block, so each has a
//! thread; the job's task drives them as `jobs::run` drives a Unix pty.

use std::io::{Read, Write};
use std::time::Duration;

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use tokio::sync::{mpsc, oneshot};

use crate::jobs::{Command, Ctl, Event, Exit, KILL_GRACE, Sink, Spec, Stdin, Stream};

fn size(rows: u16, cols: u16) -> PtySize {
    PtySize {
        rows: rows.max(1),
        cols: cols.max(1),
        pixel_width: 0,
        pixel_height: 0,
    }
}

/// What to start: the shell command's PowerShell line, or the argv list
/// (a bare `npx` found as `npx.cmd`, as `jobs::run` does).
fn builder(spec: &Spec) -> CommandBuilder {
    let mut b = match &spec.command {
        Command::Shell(s) => {
            let std = bone_lua::sys::shell(s);
            let mut b = CommandBuilder::new(std.get_program());
            for a in std.get_args() {
                b.arg(a);
            }
            b
        }
        Command::Argv(argv) => {
            let mut b = CommandBuilder::new(bone_lua::sys::program(&argv[0]));
            for a in &argv[1..] {
                b.arg(a);
            }
            b
        }
    };
    b.env("TERM", "xterm-256color");
    for (k, v) in &spec.env {
        b.env(k, v);
    }
    match &spec.cwd {
        Some(cwd) => b.cwd(cwd),
        None => {
            if let Ok(cwd) = std::env::current_dir() {
                b.cwd(cwd);
            }
        }
    }
    b
}

/// The job's task for a terminal on Windows: spawn in a pseudo console,
/// pump its output, obey the UI, report the end.
pub(crate) async fn run(id: u64, spec: Spec, mut ctl: mpsc::UnboundedReceiver<Ctl>, sink: Sink) {
    let (rows, cols) = spec.pty.unwrap_or((24, 80));
    let failed = |sink: &Sink, e: String| {
        let exit = Exit {
            error: Some(format!("cannot run {}: {e}", spec.command.display())),
            ..Default::default()
        };
        sink(id, Event::Exited(exit));
    };
    let pair = match native_pty_system().openpty(size(rows, cols)) {
        Ok(p) => p,
        Err(e) => return failed(&sink, e.to_string()),
    };
    let mut child = match pair.slave.spawn_command(builder(&spec)) {
        Ok(c) => c,
        Err(e) => return failed(&sink, e.to_string()),
    };
    // The console keeps its own copy: ours goes, so the output ends with it.
    drop(pair.slave);
    let master = pair.master;
    let (reader, writer) = match (master.try_clone_reader(), master.take_writer()) {
        (Ok(r), Ok(w)) => (r, w),
        (Err(e), _) | (_, Err(e)) => {
            let _ = child.kill();
            return failed(&sink, e.to_string());
        }
    };
    let pid = child.process_id();
    sink(id, Event::Started(pid));

    // Output: read until the console closes.
    let (read_done, read_finished) = oneshot::channel::<()>();
    {
        let sink = sink.clone();
        let mut reader: Box<dyn Read + Send> = reader;
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => sink(id, Event::Output(Stream::Stdout, buf[..n].to_vec())),
                }
            }
            let _ = read_done.send(());
        });
    }
    // Input: its own thread, so a program not reading never holds up the job.
    let (input, mut input_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    {
        let mut writer: Box<dyn Write + Send> = writer;
        std::thread::spawn(move || {
            while let Some(data) = input_rx.blocking_recv() {
                if writer.write_all(&data).and_then(|_| writer.flush()).is_err() {
                    return;
                }
            }
        });
    }
    let mut input = Some(input);
    if let Stdin::Text(text) = &spec.stdin
        && let Some(w) = &input
    {
        let _ = w.send(text.clone().into_bytes());
    }
    // The wait blocks too.
    let mut killer = child.clone_killer();
    let (waited, mut exited) = oneshot::channel();
    std::thread::spawn(move || {
        let _ = waited.send(child.wait());
    });

    let mut exit = Exit::default();
    let deadline = spec.timeout.map(|t| tokio::time::Instant::now() + t);
    let mut kill_at: Option<tokio::time::Instant> = None;
    let far = || tokio::time::Instant::now() + Duration::from_secs(86_400);
    // Windows has no polite stop for a console program: the tree goes.
    let stop = |killer: &mut Box<dyn portable_pty::ChildKiller + Send + Sync>| {
        if let Some(pid) = pid {
            bone_lua::sys::kill_tree(pid);
        }
        let _ = killer.kill();
    };
    let status = loop {
        tokio::select! {
            st = &mut exited => break st.unwrap_or_else(|_| Err(std::io::Error::other("the wait ended"))),
            Some(c) = ctl.recv() => match c {
                Ctl::Write(data) => {
                    if let Some(w) = &input {
                        let _ = w.send(data);
                    }
                }
                Ctl::Resize(rows, cols) => {
                    let _ = master.resize(size(rows, cols));
                }
                // A console's end of input is ctrl+z, enter.
                Ctl::CloseStdin => {
                    let _ = input.as_ref().map(|w| w.send(vec![0x1a, b'\r']));
                }
                Ctl::Cancel if kill_at.is_none() => {
                    exit.cancelled = true;
                    stop(&mut killer);
                    kill_at = Some(tokio::time::Instant::now() + KILL_GRACE);
                }
                Ctl::Cancel => {}
            },
            _ = tokio::time::sleep_until(deadline.unwrap_or_else(far)), if deadline.is_some() && kill_at.is_none() => {
                exit.timed_out = true;
                stop(&mut killer);
                kill_at = Some(tokio::time::Instant::now() + KILL_GRACE);
            }
            _ = tokio::time::sleep_until(kill_at.unwrap_or_else(far)), if kill_at.is_some() => {
                stop(&mut killer);
                kill_at = Some(far());
            }
        }
    };
    drop(input);
    match status {
        Ok(st) => exit.code = Some(st.exit_code() as i32),
        Err(e) => exit.error = Some(e.to_string()),
    }
    // Closing the console ends the output; what it still held comes first.
    drop(master);
    let _ = tokio::time::timeout(Duration::from_secs(1), read_finished).await;
    sink(id, Event::Exited(exit));
}

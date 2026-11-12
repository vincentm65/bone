//! `bone` command line.
// The npm package's desktop binary (`bone-gui.exe`, built with `windowed`)
// opens no console window when started from a shortcut.
#![cfg_attr(all(windows, feature = "windowed"), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use bone_core::Core;
use bone_core::scripting;
use bone_proto::{Connection, transport::default_socket_path};
use bone_server::{Listener, Server};
use bone_tui::RunOptions;

const USAGE: &str = "\
usage: bone [-r [ID]] [--connect [PATH]]
       bone --gui [--connect [PATH]]
       bone --headless [--listen [PATH]]
       bone --init

  (no options)      open the TUI on the server at the default PATH if one
                    is running; otherwise start the core as a process of
                    its own, which exits with its last client
  -r, --resume [ID] reopen session ID, or the newest session
  --connect [PATH]  open the TUI on a server started with --listen
  --gui             open the desktop GUI instead of the TUI, on the same
                    core (needs a bone built with --features gui). A GUI
                    already running opens the window there, without
                    starting over
  --headless        run the core without a UI and serve the bone API
                    (JSON-RPC 2.0, one JSON message per line) on stdin/stdout
  --exit-idle       with --listen: exit when the last client disconnects
                    (how the TUI and the GUI start their shared core)
  --listen [PATH]   with --headless: serve on a Unix socket (a named pipe on
                    Windows) instead of stdio
  --init            write starter core.lua and tui.lua (never overwrites)
  -h, --help        show this help
  -V, --version     show the version

PATH defaults to $XDG_RUNTIME_DIR/bone3/bone.sock (\\\\.\\pipe\\bone3-USERNAME
on Windows).

Configuration lives in ~/.bone (or $BONE_CONFIG_DIR):
  core.lua    providers, system prompt, Lua tools and hooks
  tui.lua     keymaps, options, commands and events for the TUI
  lua/        modules for require()
  sessions/   saved sessions

Environment variables override core.lua for one run:
  BONE_BASE_URL, BONE_MODEL       use this OpenAI-compatible endpoint and model
  BONE_API_KEY, BONE_REASONING_EFFORT, BONE_SYSTEM_PROMPT
  BONE_APPROVAL=auto              the approve plugin (if installed) never asks
  BONE_DATA_DIR                   where sessions are stored
  BONE_DEBUG_HTTP=DIR             save each model request and raw response in DIR";

#[derive(Debug, PartialEq)]
enum Mode {
    Init,
    Stdio,
    Listen { path: PathBuf, exit_idle: bool },
    Tui {
        connect: Option<PathBuf>,
        resume: Option<Option<String>>,
    },
    Gui {
        connect: Option<PathBuf>,
    },
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Option<Mode>, String> {
    let mut args = args.into_iter().peekable();
    let mut headless = false;
    let mut listen = None;
    let mut connect = None;
    let mut resume = None;
    let mut init = false;
    let mut gui = false;
    let mut exit_idle = false;
    while let Some(arg) = args.next() {
        let mut value = || args.next_if(|a| !a.starts_with('-'));
        match arg.as_str() {
            "--headless" => headless = true,
            "--init" => init = true,
            "--gui" => gui = true,
            "--listen" => listen = Some(value().map_or_else(default_socket_path, PathBuf::from)),
            "--exit-idle" => exit_idle = true,
            "--connect" => connect = Some(value().map_or_else(default_socket_path, PathBuf::from)),
            "-r" | "--resume" => resume = Some(value()),
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(None);
            }
            "-V" | "--version" => {
                println!("bone {}", version());
                return Ok(None);
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    if init {
        return Ok(Some(Mode::Init));
    }
    if headless {
        if gui {
            return Err("--gui and --headless are different ways to run bone".into());
        }
        if connect.is_some() || resume.is_some() {
            return Err("--connect and --resume are for the TUI, not --headless".into());
        }
        return Ok(Some(match listen {
            Some(path) => Mode::Listen { path, exit_idle },
            None if exit_idle => return Err("--exit-idle needs --listen".into()),
            None => Mode::Stdio,
        }));
    }
    if listen.is_some() || exit_idle {
        return Err("--listen needs --headless".into());
    }
    if gui {
        if resume.is_some() {
            return Err("--resume is for the TUI, not --gui".into());
        }
        return Ok(Some(Mode::Gui { connect }));
    }
    Ok(Some(Mode::Tui { connect, resume }))
}

/// Keep glibc's malloc from holding on to memory bone has freed. Every
/// model call builds and drops copies of the whole transcript, each a
/// little larger than the last; with an arena per thread and a mmap
/// threshold that rises after each large free, those copies are carved
/// from the heap and never returned, so a long session's RSS grows to
/// several times what it uses. A fixed threshold sends large blocks to
/// mmap, which gives them back when freed. `MALLOC_*` variables set by the
/// user win.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn tune_malloc() {
    for (var, param, value) in [
        ("MALLOC_ARENA_MAX", libc::M_ARENA_MAX, 2),
        ("MALLOC_MMAP_THRESHOLD_", libc::M_MMAP_THRESHOLD, 128 << 10),
        ("MALLOC_TRIM_THRESHOLD_", libc::M_TRIM_THRESHOLD, 128 << 10),
    ] {
        if std::env::var_os(var).is_none() {
            // SAFETY: called first thing in `main`, before other threads.
            unsafe { libc::mallopt(param, value) };
        }
    }
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn tune_malloc() {}

/// Windows has no `HOME`, but plugins and the programs bone runs read it
/// (`os.getenv("HOME")`, `~` paths): give it the user's profile folder.
fn default_home() {
    if cfg!(windows)
        && std::env::var_os("HOME").is_none_or(|h| h.is_empty())
        && let Some(profile) = std::env::var_os("USERPROFILE").filter(|p| !p.is_empty())
    {
        // SAFETY: called first in `main`, before any other thread exists.
        unsafe { std::env::set_var("HOME", profile) };
    }
}

/// Started from inside npm's packages (`bone-agent-*`), by the `bone`
/// launcher or straight from a desktop shortcut: an npm install, which
/// updates through npm (`BONE_INSTALL_KIND`, read by bone.update).
fn npm_install_kind() {
    if std::env::var_os("BONE_INSTALL_KIND").is_some() {
        return;
    }
    let in_npm = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.canonicalize().ok())
        .is_some_and(|exe| exe.components().any(|c| c.as_os_str() == "node_modules"));
    if in_npm {
        // SAFETY: called first in `main`, before any other thread exists.
        unsafe { std::env::set_var("BONE_INSTALL_KIND", "npm") };
    }
}

fn main() -> ExitCode {
    default_home();
    npm_install_kind();
    if cfg!(all(windows, feature = "windowed")) {
        bone_lua::sys::set_no_console();
    }
    tune_malloc();
    if std::env::args_os().len() == 2
        && std::env::args_os()
            .nth(1)
            .is_some_and(|a| a == "--clipboard-read")
    {
        return bone_tui::run_clipboard_helper();
    }
    let mode = match parse_args(std::env::args().skip(1)) {
        Ok(Some(mode)) => mode,
        Ok(None) => return ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("bone: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    // A GUI that is already up just shows its window. This is the whole
    // launch: no runtime, no core, no GPU.
    #[cfg(feature = "gui")]
    if let Mode::Gui { connect: None } = &mode
        && !private_core(|k| std::env::var_os(k))
        && bone_gui::request_window()
    {
        return ExitCode::SUCCESS;
    }
    let config_dir = bone_lua::config_dir();
    if let Err(e) = bone_lua::write_docs(&config_dir) {
        eprintln!(
            "bone: cannot write {}: {e}",
            config_dir.join("docs").display()
        );
    }
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    // The TUI and a stdio core end on these signals rather than dying, so
    // their shell jobs (in process groups of their own) end with them. Not
    // the GUI: its thread is busy, so a handler would never run. Windows
    // ends its console's processes itself.
    let stops = cfg!(unix) && matches!(mode, Mode::Stdio | Mode::Tui { .. });
    let code = match runtime.block_on(async {
        // Only when used: a registered handler replaces the default action.
        match if stops { stop_signal().ok() } else { None } {
            Some(stop) => tokio::select! { r = run(mode, config_dir) => r, _ = stop => Ok(()) },
            None => run(mode, config_dir).await,
        }
    }) {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, why)) => {
            eprintln!("bone: {why}");
            ExitCode::from(code)
        }
    };
    bone_core::tools::kill_all_jobs();
    // Stdin's blocking reader thread may still be waiting for input; don't
    // let runtime shutdown wait on it.
    runtime.shutdown_background();
    code
}

/// Resolves on SIGINT, SIGTERM or SIGHUP (Ctrl+C, or the terminal or
/// console closing).
fn stop_signal() -> std::io::Result<impl Future<Output = ()>> {
    #[cfg(unix)]
    let (mut int, mut term, mut hup) = {
        use tokio::signal::unix::{SignalKind, signal};
        // One the parent ignores (nohup's SIGHUP) stays ignored.
        let watch = |kind: SignalKind| {
            (!bone_lua::sys::signal_ignored(kind.as_raw_value()))
                .then(|| signal(kind))
                .transpose()
        };
        (
            watch(SignalKind::interrupt())?,
            watch(SignalKind::terminate())?,
            watch(SignalKind::hangup())?,
        )
    };
    #[cfg(windows)]
    let (mut int, mut term) = {
        use tokio::signal::windows::{ctrl_c, ctrl_close};
        (ctrl_c()?, ctrl_close()?)
    };
    Ok(async move {
        #[cfg(unix)]
        let (int, term, hup) = (recv(&mut int), recv(&mut term), recv(&mut hup));
        #[cfg(windows)]
        let (int, term, hup) = (int.recv(), term.recv(), std::future::pending::<()>());
        tokio::select! {
            _ = int => {}
            _ = term => {}
            _ = hup => {}
        }
    })
}

#[cfg(unix)]
async fn recv(signal: &mut Option<tokio::signal::unix::Signal>) {
    match signal {
        Some(signal) => drop(signal.recv().await),
        None => std::future::pending().await,
    }
}

/// Private config, sessions or per-run core overrides must not use a shared core.
fn private_core(env: impl Fn(&str) -> Option<std::ffi::OsString>) -> bool {
    [
        "BONE_CONFIG_DIR",
        "BONE_DATA_DIR",
        "BONE_MODEL",
        "BONE_BASE_URL",
        "BONE_API_KEY",
        "BONE_REASONING_EFFORT",
        "BONE_SYSTEM_PROMPT",
        "BONE_APPROVAL",
        "BONE_DEBUG_HTTP",
    ]
    .iter()
    .any(|name| env(name).is_some_and(|value| !value.is_empty()))
}

/// The version, plus the git commit it was built from when known.
fn version() -> String {
    match bone_lua::GIT_COMMIT {
        "" => env!("CARGO_PKG_VERSION").to_owned(),
        c => format!("{} ({c})", env!("CARGO_PKG_VERSION")),
    }
}

const TEMPLATES: &[(&str, &str)] = &[
    ("core.lua", include_str!("../templates/core.lua")),
    ("tui.lua", include_str!("../templates/tui.lua")),
];

/// Write starter config files that do not exist yet.
fn init_config(dir: &Path) -> StartupResult<()> {
    std::fs::create_dir_all(dir)
        .map_err(|e| (1, format!("cannot create {}: {e}", dir.display())))?;
    for (name, body) in TEMPLATES {
        let path = dir.join(name);
        if path.exists() {
            println!("kept    {} (already exists)", path.display());
            continue;
        }
        std::fs::write(&path, body)
            .map_err(|e| (1, format!("cannot write {}: {e}", path.display())))?;
        println!("created {}", path.display());
    }
    println!(
        "Next: pick a provider in {}, then run bone.",
        dir.join("core.lua").display()
    );
    Ok(())
}

type StartupResult<T> = Result<T, (u8, String)>;

fn server(config_dir: &Path) -> StartupResult<Server> {
    scripting::load(config_dir)
        .map(|loaded| Server::new(Arc::new(Core::from_loaded(loaded))))
        .map_err(|mut why| {
            if !config_dir.join("core.lua").exists() {
                why += &format!(
                    "\nRun `bone --init` to create a starter config in {}.",
                    config_dir.display()
                );
            }
            (2, why)
        })
}

/// Exit-idle: the first client, or nobody coming at all, then the last
/// client leaving. The socket file goes the moment the count hits zero,
/// so a launch landing in the teardown starts its own core instead of
/// connecting to a dying one; the grace then covers a connection that
/// was accepted before the file went but not counted yet, and such a
/// straggler keeps this core serving until it leaves too.
async fn wait_idle(mut count: tokio::sync::watch::Receiver<usize>, listener: &Listener) {
    let grace = std::time::Duration::from_millis(300);
    if tokio::time::timeout(std::time::Duration::from_secs(10), count.wait_for(|n| *n > 0))
        .await
        .is_err()
    {
        return;
    }
    loop {
        if count.wait_for(|n| *n == 0).await.is_err() {
            return;
        }
        listener.unlink();
        tokio::time::sleep(grace).await;
        if *count.borrow_and_update() == 0 {
            return;
        }
    }
}

/// Start the shared core as its own process serving `path`, and connect to
/// it. The core exits with its last client. Two launches may race: the
/// loser's core finds the socket taken and leaves, and both connect to the
/// winner's.
async fn spawn_core(path: &Path) -> StartupResult<Connection> {
    let exe = std::env::current_exe()
        .map_err(|e| (1, format!("cannot find this bone to start the core: {e}")))?;
    // The core's stderr goes to a log beside the socket: read back if it
    // dies at startup, and still writable after this launcher exits — a
    // pipe would close with it, and a later eprintln! in the core would
    // panic on the broken pipe.
    let log = path.with_file_name("core.log");
    if let Some(dir) = log.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let log_file = std::fs::File::create(&log)
        .map_err(|e| (1, format!("cannot write {}: {e}", log.display())))?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--headless")
        .arg("--listen")
        .arg(path)
        .arg("--exit-idle")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(log_file);
    // Its own group: the terminal's signals are for the UI, not the core.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut cmd, 0x0000_0208);
    let mut child = cmd
        .spawn()
        .map_err(|e| (1, format!("cannot start the core: {e}")))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match bone_proto::client::connect(path).await {
            Ok(conn) => {
                // Reap it when it exits, instead of leaving a zombie.
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                return Ok(conn);
            }
            Err(e) if matches!(child.try_wait(), Ok(Some(_))) => {
                // It lost a race to another core: connect to that one.
                if let Ok(conn) = bone_proto::client::connect(path).await {
                    return Ok(conn);
                }
                let mut why = std::fs::read_to_string(&log).unwrap_or_default();
                if why.trim().is_empty() {
                    why = format!("cannot connect: {e}");
                }
                return Err((1, format!("the core exited at startup: {}", why.trim())));
            }
            Err(e) if std::time::Instant::now() > deadline => {
                let _ = child.kill();
                return Err((1, format!("cannot connect to the core at {}: {e}", path.display())));
            }
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(30)).await,
        }
    }
}

async fn run(mode: Mode, config_dir: PathBuf) -> StartupResult<()> {
    match mode {
        // stdout belongs to the protocol from here on.
        Mode::Init => init_config(&config_dir)?,
        Mode::Stdio => server(&config_dir)?.serve_stdio().await,
        Mode::Listen { path, exit_idle } => {
            let listener = Listener::bind(&path)
                .await
                .map_err(|e| (1, format!("cannot listen on {}: {e}", path.display())))?;
            let server = server(&config_dir)?;
            eprintln!("bone: listening on {}", listener.path().display());
            let stop =
                stop_signal().map_err(|e| (1, format!("cannot install signal handlers: {e}")))?;
            let (count, idle) = tokio::sync::watch::channel(0usize);
            tokio::select! {
                r = listener.run_counted(&server, count) => r.map_err(|e| (1, format!("accept failed: {e}")))?,
                _ = wait_idle(idle, &listener), if exit_idle => {}
                _ = stop => {}
            }
            // Dropping the listener removes the socket file.
        }
        Mode::Tui { connect, resume } => {
            let Attached {
                conn,
                reload_core,
                path,
            } = attach(connect, &config_dir).await?;
            let tui = bone_tui::run(
                conn,
                RunOptions {
                    clipboard_executable: None,
                    cwd: cwd(),
                    config_dir: Some(config_dir),
                    resume,
                    reload_core,
                    reconnect: if reload_core { None } else { path },
                },
            );
            if let Some(why) = tui.await.map_err(|e| (1, e.to_string()))? {
                return Err((1, why));
            }
        }
        Mode::Gui { connect } => {
            // Only a GUI on the default core serves the control socket
            // another `bone --gui` opens its window through.
            let resident = connect.is_none();
            gui(attach(connect, &config_dir).await?, config_dir, resident).await?
        }
    }
    Ok(())
}

/// A frontend's connection to its core.
struct Attached {
    conn: Connection,
    /// The core runs in this process, so the frontend may reload it.
    reload_core: bool,
    /// The default or `--connect` socket.
    path: Option<PathBuf>,
}

/// Connect a frontend to the core at `connect`, or the default socket.
async fn attach(connect: Option<PathBuf>, config_dir: &Path) -> StartupResult<Attached> {
    // Explicit connects never start a core. Plain bone attaches to the
    // default socket, starting the core there if none is, except when this
    // run has private core settings: that core stays in-process.
    let explicit = connect.is_some();
    let Some(path) =
        connect.or_else(|| (!private_core(|k| std::env::var_os(k))).then(default_socket_path))
    else {
        // core.lua loads only here, where this run's core actually lives.
        return Ok(Attached {
            conn: server(config_dir)?.connect_in_process(),
            reload_core: true,
            path: None,
        });
    };
    let conn = match bone_proto::client::connect(&path).await {
        Ok(conn) => conn,
        Err(e) if explicit => {
            return Err((1, format!("cannot connect to {}: {e}", path.display())));
        }
        // On Windows, another user's bone (or an older one run as
        // administrator) owns the pipe; a core we start is doomed too.
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            return Err((
                1,
                format!(
                    "another bone is running at {} that this one may not join (another \
                     user's, or an older bone run as administrator); quit it and start \
                     bone again",
                    path.display()
                ),
            ));
        }
        Err(_) => spawn_core(&path).await?,
    };
    Ok(Attached {
        conn,
        reload_core: false,
        path: Some(path),
    })
}

fn cwd() -> String {
    std::env::current_dir()
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".into())
}

/// The GUI's core, like the TUI's, runs in its own process and exits with
/// its last client.
#[cfg(feature = "gui")]
async fn gui(a: Attached, config_dir: PathBuf, resident: bool) -> StartupResult<()> {
    let resident = resident && a.path.is_some();
    let opts = bone_gui::GuiOptions {
        cwd: cwd(),
        reconnect: if a.reload_core { None } else { a.path },
        config_dir: Some(config_dir),
    };
    bone_gui::run_with(a.conn, opts, resident)
        .await
        .map_err(|why| (1, why))
}

#[cfg(not(feature = "gui"))]
async fn gui(_: Attached, _: PathBuf, _: bool) -> StartupResult<()> {
    Err((
        2,
        "this bone was built without the GUI; build it with --features gui".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Option<Mode>, String> {
        parse_args(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn private_overrides_are_nonempty_and_core_only() {
        let names = [
            "BONE_CONFIG_DIR",
            "BONE_DATA_DIR",
            "BONE_MODEL",
            "BONE_BASE_URL",
            "BONE_API_KEY",
            "BONE_REASONING_EFFORT",
            "BONE_SYSTEM_PROMPT",
            "BONE_APPROVAL",
            "BONE_DEBUG_HTTP",
        ];
        assert!(!private_core(|_| None));
        assert!(!private_core(|_| Some("".into())));
        for name in names {
            assert!(
                private_core(|key| (key == name).then(|| "value".into())),
                "{name}"
            );
        }
        assert!(!private_core(
            |key| (key == "BONE_UNRELATED").then(|| "value".into())
        ));
    }

    #[test]
    fn args() {
        let tui = |connect: Option<&str>, resume: Option<Option<&str>>| {
            Ok(Some(Mode::Tui {
                connect: connect.map(PathBuf::from),
                resume: resume.map(|r| r.map(str::to_owned)),
            }))
        };
        assert_eq!(parse(&[]), tui(None, None));
        assert_eq!(parse(&["-r"]), tui(None, Some(None)));
        assert_eq!(
            parse(&["--resume", "abc", "--connect"]),
            tui(
                Some(default_socket_path().to_str().unwrap()),
                Some(Some("abc"))
            )
        );
        assert_eq!(parse(&["--headless"]), Ok(Some(Mode::Stdio)));
        assert_eq!(
            parse(&["--headless", "--listen", "/tmp/s"]),
            Ok(Some(Mode::Listen { path: "/tmp/s".into(), exit_idle: false }))
        );
        assert_eq!(
            parse(&["--listen", "--headless"]),
            Ok(Some(Mode::Listen { path: default_socket_path(), exit_idle: false }))
        );
        assert!(parse(&["--listen"]).is_err());
        assert_eq!(parse(&["--init"]), Ok(Some(Mode::Init)));
        assert!(parse(&["--headless", "-r"]).is_err());
        assert!(parse(&["--headless", "--bogus"]).is_err());
    }
}

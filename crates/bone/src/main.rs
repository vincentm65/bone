//! `bone` command line.

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
                    is running; otherwise run the core in this process and
                    serve it at PATH while the TUI is open
  -r, --resume [ID] reopen session ID, or the newest session
  --connect [PATH]  open the TUI on a server started with --listen
  --gui             open the desktop GUI instead of the TUI, on the same
                    core (needs a bone built with --features gui)
  --headless        run the core without a UI and serve the bone API
                    (JSON-RPC 2.0, one JSON message per line) on stdin/stdout
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
    Listen(PathBuf),
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
    while let Some(arg) = args.next() {
        let mut value = || args.next_if(|a| !a.starts_with('-'));
        match arg.as_str() {
            "--headless" => headless = true,
            "--init" => init = true,
            "--gui" => gui = true,
            "--listen" => listen = Some(value().map_or_else(default_socket_path, PathBuf::from)),
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
        return Ok(Some(listen.map_or(Mode::Stdio, Mode::Listen)));
    }
    if listen.is_some() {
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

fn main() -> ExitCode {
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
    let config_dir = bone_lua::config_dir();
    if let Err(e) = bone_lua::write_docs(&config_dir) {
        eprintln!(
            "bone: cannot write {}: {e}",
            config_dir.join("docs").display()
        );
    }
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let code = match runtime.block_on(run(mode, config_dir)) {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, why)) => {
            eprintln!("bone: {why}");
            ExitCode::from(code)
        }
    };
    // Stdin's blocking reader thread may still be waiting for input; don't
    // let runtime shutdown wait on it.
    runtime.shutdown_background();
    code
}

/// Resolves on SIGINT or SIGTERM (Ctrl+C or the console closing on Windows).
fn stop_signal() -> std::io::Result<impl Future<Output = ()>> {
    #[cfg(unix)]
    let (mut int, mut term) = {
        use tokio::signal::unix::{SignalKind, signal};
        (
            signal(SignalKind::interrupt())?,
            signal(SignalKind::terminate())?,
        )
    };
    #[cfg(windows)]
    let (mut int, mut term) = {
        use tokio::signal::windows::{ctrl_c, ctrl_close};
        (ctrl_c()?, ctrl_close()?)
    };
    Ok(async move {
        tokio::select! {
            _ = int.recv() => {}
            _ = term.recv() => {}
        }
    })
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

enum Endpoint {
    Attached(Connection),
    Local(Option<Listener>),
}

/// Another startup may win the socket after our initial connect failed.
async fn bind_or_connect(path: &Path) -> StartupResult<Endpoint> {
    let display = path.display();
    match Listener::bind(path).await {
        Ok(listener) => Ok(Endpoint::Local(Some(listener))),
        Err(bind) => bone_client::connect(path)
            .await
            .map(Endpoint::Attached)
            .map_err(|connect| {
                (
                    1,
                    format!("cannot listen on {display}: {bind}; cannot connect: {connect}"),
                )
            }),
    }
}

async fn run(mode: Mode, config_dir: PathBuf) -> StartupResult<()> {
    match mode {
        // stdout belongs to the protocol from here on.
        Mode::Init => init_config(&config_dir)?,
        Mode::Stdio => server(&config_dir)?.serve_stdio().await,
        Mode::Listen(path) => {
            let listener = Listener::bind(&path)
                .await
                .map_err(|e| (1, format!("cannot listen on {}: {e}", path.display())))?;
            let server = server(&config_dir)?;
            eprintln!("bone: listening on {}", listener.path().display());
            let stop =
                stop_signal().map_err(|e| (1, format!("cannot install signal handlers: {e}")))?;
            tokio::select! {
                r = listener.run(&server) => r.map_err(|e| (1, format!("accept failed: {e}")))?,
                _ = stop => {}
            }
            // Dropping the listener removes the socket file.
        }
        Mode::Tui { connect, resume } => {
            let Attached {
                conn,
                shared,
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
            // Keep the listener alive only while the TUI runs, propagating errors.
            tokio::select! {
                r = tui => {
                    if let Some(why) = r.map_err(|e| (1, e.to_string()))? {
                        return Err((1, why));
                    }
                }
                r = async {
                    let (listener, server) = shared.as_ref().unwrap();
                    listener.run(server).await
                }, if shared.is_some() => r.map_err(|e| (1, format!("accept failed: {e}")))?,
            }
        }
        Mode::Gui { connect } => gui(attach(connect, &config_dir).await?).await?,
    }
    Ok(())
}

/// A frontend's connection to its core, and the core it hosts, if any.
struct Attached {
    conn: Connection,
    /// The core this process runs, and the socket it serves it on.
    shared: Option<(Listener, Server)>,
    /// The core runs in this process, so the frontend may reload it.
    reload_core: bool,
    /// The default or `--connect` socket.
    path: Option<PathBuf>,
}

/// Connect a frontend to the core at `connect`, or the default socket.
async fn attach(connect: Option<PathBuf>, config_dir: &Path) -> StartupResult<Attached> {
    // Explicit connects never start a core. Plain bone attaches or hosts
    // the default socket, except when this run has private core settings.
    let explicit = connect.is_some();
    let path =
        connect.or_else(|| (!private_core(|k| std::env::var_os(k))).then(default_socket_path));
    let endpoint = match &path {
        Some(path) => match bone_client::connect(path).await {
            Ok(conn) => Endpoint::Attached(conn),
            Err(e) if explicit => {
                return Err((1, format!("cannot connect to {}: {e}", path.display())));
            }
            Err(_) => bind_or_connect(path).await?,
        },
        None => Endpoint::Local(None),
    };
    // Loading core.lua is deliberately after deciding whether to attach.
    Ok(match endpoint {
        Endpoint::Attached(conn) => Attached {
            conn,
            shared: None,
            reload_core: false,
            path,
        },
        Endpoint::Local(listener) => {
            let server = server(config_dir)?;
            let conn = server.connect_in_process();
            Attached {
                conn,
                shared: listener.map(|listener| (listener, server)),
                reload_core: true,
                path,
            }
        }
    })
}

fn cwd() -> String {
    std::env::current_dir()
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".into())
}

/// The GUI blocks its thread, so the shared core is served from a task,
/// stopped (removing the socket) when the window closes.
#[cfg(feature = "gui")]
async fn gui(a: Attached) -> StartupResult<()> {
    let serving = a.shared.map(|(listener, server)| {
        tokio::spawn(async move {
            if let Err(e) = listener.run(&server).await {
                eprintln!("bone: accept failed: {e}");
            }
        })
    });
    let opts = bone_gui::GuiOptions {
        cwd: cwd(),
        reconnect: if a.reload_core { None } else { a.path },
    };
    let r = bone_gui::run(a.conn, opts).await;
    if let Some(task) = serving {
        task.abort();
        let _ = task.await;
    }
    r.map_err(|why| (1, why))
}

#[cfg(not(feature = "gui"))]
async fn gui(_: Attached) -> StartupResult<()> {
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

    #[cfg(unix)]
    #[tokio::test]
    async fn concurrent_startup_loser_attaches_to_winner() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bone.sock");
        let (a, b) = tokio::join!(bind_or_connect(&path), bind_or_connect(&path));
        let (listener, conn) = match (a.unwrap(), b.unwrap()) {
            (Endpoint::Local(Some(listener)), Endpoint::Attached(conn))
            | (Endpoint::Attached(conn), Endpoint::Local(Some(listener))) => (listener, conn),
            _ => panic!("expected one host and one attached client"),
        };
        // The attached connection must speak to the winning core, not a second core.
        let loaded = scripting::load_with(dir.path(), &|key| match key {
            "BONE_BASE_URL" => Some("http://127.0.0.1:9".into()),
            "BONE_MODEL" => Some("winner".into()),
            _ => None,
        })
        .unwrap();
        let server = Server::new(Arc::new(Core::from_loaded(loaded)));
        let (client, _events) = bone_client::Client::new(conn);
        tokio::select! {
            result = listener.run(&server) => panic!("listener stopped: {result:?}"),
            result = tokio::time::timeout(std::time::Duration::from_secs(5), client.initialize("loser")) => {
                assert_eq!(result.unwrap().unwrap().server_name, "bone");
            }
        }
        assert!(path.exists());
        drop(listener);
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn socket_startup_reports_bind_and_connect_failures() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not-a-socket");
        std::fs::write(&path, "keep me").unwrap();
        let Err((code, why)) = bind_or_connect(&path).await else {
            panic!("must not silently start an unshared core");
        };
        assert_eq!(code, 1);
        assert!(why.contains("cannot listen on"), "{why}");
        assert!(why.contains("cannot connect"), "{why}");
        assert_eq!(std::fs::read_to_string(path).unwrap(), "keep me");
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
            Ok(Some(Mode::Listen("/tmp/s".into())))
        );
        assert_eq!(
            parse(&["--listen", "--headless"]),
            Ok(Some(Mode::Listen(default_socket_path())))
        );
        assert!(parse(&["--listen"]).is_err());
        assert_eq!(parse(&["--init"]), Ok(Some(Mode::Init)));
        assert!(parse(&["--headless", "-r"]).is_err());
        assert!(parse(&["--headless", "--bogus"]).is_err());
    }
}

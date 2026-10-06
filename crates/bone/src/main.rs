//! `bone` command line.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use bone_core::Core;
use bone_core::scripting::{self, Loaded};
use bone_proto::transport::default_socket_path;
use bone_server::{Listener, Server};
use bone_tui::RunOptions;
use tokio::signal::unix::{SignalKind, signal};

const USAGE: &str = "\
usage: bone [-r [ID]] [--connect [PATH]]
       bone --headless [--listen [PATH]]
       bone --init
       bone --import-bone [DB]

  (no options)      open the TUI with the core running in this process
  -r, --resume [ID] reopen session ID, or the newest session
  --connect [PATH]  open the TUI on a server started with --listen
  --headless        run the core without a UI and serve the bone API
                    (JSON-RPC 2.0, one JSON message per line) on stdin/stdout
  --listen [PATH]   with --headless: serve on a Unix socket instead of stdio
  --init            write starter core.lua and tui.lua (never overwrites)
  --import-bone [DB] import the first bone's conversations as sessions
                    (DB defaults to ~/.bone-rust/data/conversations.db);
                    again later to bring them up to date
  -h, --help        show this help
  -V, --version     show the version

PATH defaults to $XDG_RUNTIME_DIR/bone3/bone.sock.

Configuration lives in ~/.bone (or $BONE_CONFIG_DIR):
  core.lua    providers, system prompt, Lua tools and hooks
  tui.lua     keymaps, options, commands and events for the TUI
  lua/        modules for require()
  sessions/   saved sessions

Environment variables override core.lua for one run:
  BONE_BASE_URL, BONE_MODEL       use this OpenAI-compatible endpoint and model
  BONE_API_KEY, BONE_REASONING_EFFORT, BONE_SYSTEM_PROMPT
  BONE_APPROVAL=auto              the approve plugin (if installed) never asks
  BONE_DATA_DIR                   where sessions are stored";

#[derive(Debug, PartialEq)]
enum Mode {
    Init,
    Import(PathBuf),
    Stdio,
    Listen(PathBuf),
    Tui {
        connect: Option<PathBuf>,
        resume: Option<Option<String>>,
    },
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Option<Mode>, String> {
    let mut args = args.into_iter().peekable();
    let mut headless = false;
    let mut listen = None;
    let mut connect = None;
    let mut resume = None;
    let mut init = false;
    let mut import = None;
    while let Some(arg) = args.next() {
        let mut value = || args.next_if(|a| !a.starts_with('-'));
        match arg.as_str() {
            "--headless" => headless = true,
            "--init" => init = true,
            "--import-bone" => import = Some(value().map_or_else(default_bone_db, PathBuf::from)),
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
    if let Some(db) = import {
        return Ok(Some(Mode::Import(db)));
    }
    if headless {
        if connect.is_some() || resume.is_some() {
            return Err("--connect and --resume are for the TUI, not --headless".into());
        }
        return Ok(Some(listen.map_or(Mode::Stdio, Mode::Listen)));
    }
    if listen.is_some() {
        return Err("--listen needs --headless".into());
    }
    Ok(Some(Mode::Tui { connect, resume }))
}

fn main() -> ExitCode {
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
    if mode == Mode::Init {
        return init_config(&config_dir);
    }
    // Only modes that run the core need its configuration.
    let loaded = match &mode {
        Mode::Tui {
            connect: Some(_), ..
        } => None,
        _ => match scripting::load(&config_dir) {
            Ok(l) => Some(l),
            Err(e) => {
                eprintln!("bone: {e}");
                if !config_dir.join("core.lua").exists() {
                    eprintln!(
                        "Run `bone --init` to create a starter config in {}.",
                        config_dir.display()
                    );
                }
                return ExitCode::from(2);
            }
        },
    };
    if let (Mode::Import(db), Some(loaded)) = (&mode, &loaded) {
        return import_bone(db, &loaded.config.data_dir);
    }
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let code = runtime.block_on(run(mode, loaded, config_dir));
    // Stdin's blocking reader thread may still be waiting for input; don't
    // let runtime shutdown wait on it.
    runtime.shutdown_background();
    code
}

fn default_bone_db() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".bone-rust/data/conversations.db")
}

/// `--import-bone`: the first bone's conversations, as sessions.
fn import_bone(db: &std::path::Path, data_dir: &std::path::Path) -> ExitCode {
    use std::io::Write;
    eprintln!("Importing {} into {}", db.display(), data_dir.display());
    let mut shown = usize::MAX;
    let progress = |done: usize, total: usize| {
        let pct = (done * 100).checked_div(total).unwrap_or(100);
        if pct != shown {
            shown = pct;
            eprint!("\r  {done}/{total} conversations ({pct}%)");
            let _ = std::io::stderr().flush();
        }
    };
    match bone_core::import::import_bone1(db, data_dir, progress) {
        Ok(r) => {
            eprintln!();
            println!(
                "{} imported, {} updated, {} unchanged, {} continued here and left alone; {} messages written.",
                r.imported, r.updated, r.unchanged, r.kept, r.messages
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("\nbone: import failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The version, plus the git commit it was built from when known.
fn version() -> String {
    match option_env!("BONE_GIT_COMMIT").filter(|c| !c.is_empty()) {
        Some(c) => format!("{} ({c})", env!("CARGO_PKG_VERSION")),
        None => env!("CARGO_PKG_VERSION").to_owned(),
    }
}

const TEMPLATES: &[(&str, &str)] = &[
    ("core.lua", include_str!("../templates/core.lua")),
    ("tui.lua", include_str!("../templates/tui.lua")),
];

/// Write starter config files that do not exist yet.
fn init_config(dir: &std::path::Path) -> ExitCode {
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("bone: cannot create {}: {e}", dir.display());
        return ExitCode::FAILURE;
    }
    for (name, body) in TEMPLATES {
        let path = dir.join(name);
        if path.exists() {
            println!("kept    {} (already exists)", path.display());
            continue;
        }
        if let Err(e) = std::fs::write(&path, body) {
            eprintln!("bone: cannot write {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
        println!("created {}", path.display());
    }
    println!(
        "Next: pick a provider in {}, then run bone.",
        dir.join("core.lua").display()
    );
    ExitCode::SUCCESS
}

async fn run(mode: Mode, loaded: Option<Loaded>, config_dir: PathBuf) -> ExitCode {
    let mut loaded = loaded;
    let mut server = || {
        Server::new(Arc::new(Core::from_loaded(
            loaded.take().expect("config loaded"),
        )))
    };
    match mode {
        // stdout belongs to the protocol from here on.
        Mode::Init | Mode::Import(_) => unreachable!("handled before running the core"),
        Mode::Stdio => server().serve_stdio().await,
        Mode::Listen(path) => {
            let server = server();
            let listener = match Listener::bind(&path).await {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("bone: cannot listen on {}: {e}", path.display());
                    return ExitCode::FAILURE;
                }
            };
            eprintln!("bone: listening on {}", listener.path().display());
            let (Ok(mut int), Ok(mut term)) = (
                signal(SignalKind::interrupt()),
                signal(SignalKind::terminate()),
            ) else {
                eprintln!("bone: cannot install signal handlers");
                return ExitCode::FAILURE;
            };
            tokio::select! {
                r = listener.run(&server) => {
                    if let Err(e) = r {
                        eprintln!("bone: accept failed: {e}");
                        return ExitCode::FAILURE;
                    }
                }
                _ = int.recv() => {}
                _ = term.recv() => {}
            }
            // Dropping the listener removes the socket file.
        }
        Mode::Tui { connect, resume } => {
            let reload_core = connect.is_none();
            let conn = match connect {
                Some(path) => match bone_client::connect_unix(&path).await {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("bone: cannot connect to {}: {e}", path.display());
                        return ExitCode::FAILURE;
                    }
                },
                None => server().connect_in_process(),
            };
            let cwd = std::env::current_dir()
                .map(|d| d.to_string_lossy().into_owned())
                .unwrap_or_else(|_| ".".into());
            match bone_tui::run(
                conn,
                RunOptions {
                    cwd,
                    config_dir: Some(config_dir),
                    resume,
                    reload_core,
                },
            )
            .await
            {
                Ok(None) => {}
                Ok(Some(why)) => {
                    eprintln!("bone: {why}");
                    return ExitCode::FAILURE;
                }
                Err(e) => {
                    eprintln!("bone: {e}");
                    return ExitCode::FAILURE;
                }
            }
        }
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Option<Mode>, String> {
        parse_args(args.iter().map(|s| s.to_string()))
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
        assert_eq!(
            parse(&["--import-bone", "/x.db"]),
            Ok(Some(Mode::Import("/x.db".into())))
        );
        assert_eq!(
            parse(&["--import-bone"]),
            Ok(Some(Mode::Import(default_bone_db())))
        );
        assert!(parse(&["--headless", "-r"]).is_err());
        assert!(parse(&["--headless", "--bogus"]).is_err());
    }
}

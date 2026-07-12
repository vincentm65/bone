//! Binary entry point: arg parsing, provider setup, and TUI / headless dispatch.

use bone::config::{UserConfig, custom::CustomConfigs};
use bone::llm::provider::LlmProvider;
use bone::llm::providers;
use bone::run;
use bone::ui::app::App;

/// Wrap a future so a panic inside it is logged instead of silently killing
/// the spawned task.
///
/// Now that `panic = "abort"` is gone, an unguarded `tokio::spawn` task that
/// panics simply vanishes — the JoinHandle is never awaited, so nothing
/// surfaces the death. This helper turns those silent deaths into a visible
/// `eprintln!` so the operator at least knows *which* task died.
async fn panic_guard<F>(label: &'static str, fut: F)
where
    F: std::future::Future,
{
    use futures_util::FutureExt;
    use std::panic::AssertUnwindSafe;
    if let Err(payload) = AssertUnwindSafe(fut).catch_unwind().await {
        eprintln!(
            "bone: fatal: {label} task panicked: {}",
            bone::runtime::panic_message(&*payload)
        );
    }
}
struct CliOptions {
    provider: Option<String>,
    model: Option<String>,
    /// `--connect <addr>`: run the full TUI against a remote `bone serve`
    /// daemon instead of an in-process one.
    connect: Option<String>,
    changed: bool,
}

fn parse_cli_options(args: &[String]) -> Result<CliOptions, String> {
    let mut provider = None;
    let mut model = None;
    let mut connect = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--provider" => {
                i += 1;
                provider = Some(args.get(i).ok_or("--provider requires a value")?.clone());
            }
            "--model" => {
                i += 1;
                model = Some(args.get(i).ok_or("--model requires a value")?.clone());
            }
            "--connect" => {
                i += 1;
                connect = Some(args.get(i).ok_or("--connect requires an address")?.clone());
            }
            "--help" | "-h" => return Err(usage()),
            other => return Err(format!("unknown argument: {other}\n{}", usage())),
        }
        i += 1;
    }

    let changed = provider.is_some() || model.is_some();
    Ok(CliOptions {
        provider,
        model,
        connect,
        changed,
    })
}

/// Deps that bone tools need at runtime. None are required for the base app.
struct Dep {
    bin: &'static str,
    /// Package name if different from binary name. None = same as bin.
    pkg: Option<&'static str>,
    label: &'static str,
}

const DEPS: &[Dep] = &[
    Dep {
        bin: "uv",
        pkg: None,
        label: "uv (needed by web_search, task_list, cron, browser/browser-use)",
    },
    Dep {
        bin: "git",
        pkg: None,
        label: "git (needed by /r command and git workflow)",
    },
    Dep {
        bin: "sqlite3",
        pkg: Some("sqlite3"),
        label: "sqlite3 (needed by /memory command)",
    },
];

fn have_bin(name: &str) -> bool {
    std::process::Command::new(name)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

fn try_install(bin: &str, pkg: Option<&str>) -> bool {
    let pkg = pkg.unwrap_or(bin);

    // macOS: brew
    if cfg!(target_os = "macos") && have_bin("brew") {
        return run_silent("brew", &["install", pkg]);
    }

    // Linux package managers
    if cfg!(target_os = "linux") {
        if have_bin("apt-get") {
            return run_silent("sudo", &["apt-get", "install", "-y", pkg]);
        }
        if have_bin("dnf") {
            return run_silent("sudo", &["dnf", "install", "-y", pkg]);
        }
        if have_bin("pacman") {
            return run_silent("sudo", &["pacman", "-S", "--noconfirm", pkg]);
        }
        if have_bin("apk") {
            return run_silent("apk", &["add", pkg]);
        }
    }

    // Windows: winget
    if cfg!(windows) && have_bin("winget") {
        let winget_id = match bin {
            "uv" => "astral-sh.uv",
            "git" => "Git.Git",
            "sqlite3" => return false,
            _ => return false,
        };
        return run_silent(
            "winget",
            &[
                "install",
                "--id",
                winget_id,
                "-e",
                "--source",
                "winget",
                "--accept-package-agreements",
            ],
        );
    }

    // Fallback for uv: official installer
    if bin == "uv" {
        if cfg!(windows) {
            return run_silent(
                "powershell",
                &[
                    "-ExecutionPolicy",
                    "ByPass",
                    "-c",
                    "irm https://astral.sh/uv/install.ps1 | iex",
                ],
            );
        }
        return run_silent(
            "sh",
            &["-c", "curl -LsSf https://astral.sh/uv/install.sh | sh"],
        );
    }

    false
}

fn run_silent(program: &str, args: &[&str]) -> bool {
    std::process::Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn ensure_deps() {
    // Only check/install once
    let marker = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".bone-rust")
        .join(".deps-warned");
    if marker.exists() {
        return;
    }

    let mut missing = Vec::new();
    for dep in DEPS {
        if have_bin(dep.bin) {
            continue;
        }
        eprintln!("bone: installing {} ...", dep.bin);
        if try_install(dep.bin, dep.pkg) && have_bin(dep.bin) {
            eprintln!("bone: installed {}.", dep.label);
        } else {
            missing.push(dep);
        }
    }

    if !missing.is_empty() {
        eprintln!();
        eprintln!("bone: couldn't auto-install:");
        for dep in missing {
            eprintln!("  - {}: {}", dep.bin, dep.label);
        }
        eprintln!(
            "The base app works without these. They're only needed for the tools listed above."
        );
    }

    let _ = std::fs::write(&marker, "");
}

fn usage() -> String {
    "Usage: bone [--provider <id>] [--model <name>]\n       bone --connect <addr>          # run the TUI against a remote `bone serve`\n       bone agent [--provider <id>] [--model <name>] ...\n       bone serve [--listen <addr>]   # run as a daemon (default 127.0.0.1:7878)\n       bone connect [--listen <addr>] # line-oriented RPC client\n       bone web                       # launch the web UI (http://localhost:4577)".to_string()
}

/// Parse `--listen <addr>` from args, falling back to the default.
fn parse_listen_addr(args: &[String]) -> String {
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--listen"
            && let Some(v) = args.get(i + 1)
        {
            return v.clone();
        }
        i += 1;
    }
    "127.0.0.1:7878".to_string()
}

/// Tolerantly pull `--provider <id>` / `--model <name>` out of a subcommand's
/// args (ignores everything else, unlike [`parse_cli_options`] which rejects
/// unknown flags). Used by `bone serve` to honor provider/model overrides.
fn parse_provider_model(args: &[String]) -> (Option<String>, Option<String>) {
    let (mut provider, mut model) = (None, None);
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--provider" => provider = args.get(i + 1).cloned(),
            "--model" => model = args.get(i + 1).cloned(),
            _ => {}
        }
        i += 1;
    }
    (provider, model)
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

/// Resolve when stdin reaches EOF. Parent-managed `bone serve` processes can
/// opt into this so they shut down when the parent closes their stdin pipe.
async fn wait_stdin_eof() {
    use tokio::io::AsyncReadExt;
    let mut stdin = tokio::io::stdin();
    let mut buf = [0_u8; 256];
    loop {
        match stdin.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
    }
}

/// Fully booted runtime host: provider, Lua extension manager, and session.
struct RuntimeHostBoot {
    provider: std::sync::Arc<dyn bone::llm::provider::LlmProvider>,
    manager: bone::ext::ExtensionManager,
    session: std::sync::Arc<std::sync::Mutex<bone::runtime::RuntimeSession>>,
}

/// Boot the runtime host: create tools, init the session DB, and return
/// the pieces a daemon (in-process or serve) needs. `target` selects which
/// durable conversation the session attaches to (fresh, latest, or a specific
/// id).
fn boot_runtime_host_for(
    provider: std::sync::Arc<dyn bone::llm::provider::LlmProvider>,
    custom: &mut CustomConfigs,
    target: bone::rpc::SessionTarget,
) -> std::io::Result<RuntimeHostBoot> {
    let model = provider.model().to_string();
    let provider_label = format!("{} ({})", provider.name(), provider.id());
    let booted = bone::ext::boot_with_tools(
        &bone::config::bone_dir(),
        &std::env::current_dir()?,
        custom,
        true,
        bone::ext::BootOptions {
            agent_depth: 0,
            headless: false,
            model: model.clone(),
            provider: provider_label.clone(),
            tool_allowlist: None,
        },
        &model,
        &provider_label,
    );
    let mut session = bone::runtime::RuntimeSession::new(booted.tools);
    let warning = match target {
        bone::rpc::SessionTarget::Latest => session.init_db(&*provider),
        bone::rpc::SessionTarget::New => session.init_db_new(&*provider),
        bone::rpc::SessionTarget::Conversation(id) => session.init_db_conversation(&*provider, id),
    };
    if let Some(warning) = warning {
        return Err(std::io::Error::other(warning));
    }
    Ok(RuntimeHostBoot {
        provider,
        manager: booted.manager,
        session: std::sync::Arc::new(std::sync::Mutex::new(session)),
    })
}

/// `bone serve` — bind a socket, run the headless runtime, and fan its events
/// out to every attached frontend. Each `SubmitPrompt` drives a full agent turn
/// whose events stream back over the protocol.
async fn run_serve(args: &[String]) -> std::io::Result<()> {
    let addr = parse_listen_addr(args);
    let shutdown_on_eof = has_flag(args, "--shutdown-on-stdin-eof");
    // Build the provider from config, honoring `--provider`/`--model`.
    let (cli_provider, cli_model) = parse_provider_model(args);
    let mut custom = CustomConfigs::load();
    let provider_id = cli_provider.unwrap_or_else(|| {
        if custom.get_last_provider().is_empty() {
            "local".to_string()
        } else {
            custom.get_last_provider()
        }
    });
    if let Some(model) = cli_model.as_ref()
        && let Some(mut entry) = custom.get_provider_entry("providers", &provider_id)
    {
        entry.model = model.clone();
        custom.set_provider_entry("providers", &provider_id, &entry);
    }
    let providers_config = custom.derive_providers_config();
    let provider = providers::create_provider_with_config(&provider_id, &providers_config)
        .map_err(std::io::Error::other)?;
    let provider: std::sync::Arc<dyn bone::llm::provider::LlmProvider> =
        std::sync::Arc::from(provider);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    eprintln!("bone: serving runtime on {addr} (provider: {provider_id})");

    let approval_mode = match custom.get_value("general", "approval_mode").as_str() {
        "danger" => bone::tools::ApprovalMode::Danger,
        _ => bone::tools::ApprovalMode::Safe,
    };
    let (session_manager, manager_rx) = bone::rpc::SessionManager::new();
    let factory_provider = provider.clone();
    let factory = move |target: bone::rpc::SessionTarget| {
        // Each actor gets an independent Lua VM/tool state and RuntimeSession.
        // The provider HTTP client is safe to share and each actor may still
        // switch its own provider later through the existing command.
        let mut custom = CustomConfigs::load();
        let mut boot = boot_runtime_host_for(factory_provider.clone(), &mut custom, target)
            .map_err(|err| err.to_string())?;
        let conversation_id = boot
            .session
            .lock()
            .unwrap()
            .conversation_id
            .ok_or_else(|| "runtime has no durable conversation id".to_string())?;
        // Each conversation actor should report and use the provider/model this
        // conversation was created with, not the daemon's boot default. Look up
        // the stored pair and rebuild the provider when it differs. (A brand-new
        // conversation was just minted with the boot provider, so it matches and
        // this is a no-op.)
        if let Some((want_provider, want_model)) =
            bone::session_db::SessionDb::open(&bone::session_db::db_path())
                .ok()
                .and_then(|db| {
                    db.conversation_provider_model(conversation_id)
                        .ok()
                        .flatten()
                })
        {
            let matches =
                want_provider == boot.provider.id() && want_model == boot.provider.model();
            if !matches {
                let providers_config = custom.derive_providers_config();
                match bone::llm::providers::build_provider(
                    &want_provider,
                    &want_model,
                    &providers_config,
                ) {
                    Ok(p) => boot.provider = std::sync::Arc::from(p),
                    Err(err) => eprintln!(
                        "bone: warning: conversation {conversation_id} wants provider \
                         `{want_provider}` but it could not be built ({err}); \
                         using {}",
                        boot.provider.id()
                    ),
                }
            }
        }
        let frontend =
            bone::rpc::frontend_state(&boot.manager, &boot.session.lock().unwrap().tools);
        let sync_session = boot.session.clone();
        let sync_provider = boot.provider.clone();
        let initial = std::sync::Arc::new(move || {
            let session = sync_session.lock().unwrap();
            let snapshot = session.snapshot(sync_provider.id(), sync_provider.model());
            vec![
                frontend.clone(),
                bone::runtime::RuntimeEvent::StateSnapshot {
                    snapshot: snapshot.clone(),
                },
                // Always send this, including for an empty new conversation,
                // so switching actors clears stale frontend scrollback.
                bone::runtime::RuntimeEvent::ConversationLoaded {
                    messages: session.display_transcript(),
                    snapshot,
                },
            ]
        });
        let (hub, commands_rx) = bone::rpc::Hub::new();
        let task = Box::pin(bone::rpc::run_daemon(
            hub.publisher(),
            commands_rx,
            boot.provider,
            boot.manager,
            boot.session,
            approval_mode,
            None,
            true,
            // Remote clients of `bone serve` cannot self-inject background
            // sub-agent results / `bone.api.submit` prompts, so the daemon does.
            true,
        ));
        Ok(bone::rpc::ManagedRuntime {
            conversation_id,
            hub,
            initial,
            task,
        })
    };

    let accept_manager = session_manager.clone();
    let accept_loop = async move {
        loop {
            match listener.accept().await {
                Ok((stream, peer)) => {
                    eprintln!("bone: client attached: {peer}");
                    let manager = accept_manager.clone();
                    tokio::spawn(panic_guard("client", async move {
                        if let Err(e) = bone::rpc::serve_managed_connection(
                            stream,
                            manager,
                            bone::rpc::SessionTarget::Latest,
                        )
                        .await
                        {
                            eprintln!("bone: client {peer} ended: {e}");
                        }
                    }));
                }
                Err(e) => {
                    eprintln!("bone: accept error: {e}");
                    break;
                }
            }
        }
    };

    tokio::select! {
        _ = bone::rpc::run_session_manager(manager_rx, factory) => {
            eprintln!("bone: session manager exited; shutting down server")
        },
        _ = accept_loop => eprintln!("bone: accept loop ended; shutting down server"),
        _ = wait_stdin_eof(), if shutdown_on_eof => {
            eprintln!("bone: parent stdin closed; shutting down server")
        }
    }
    Ok(())
}

/// `bone web` — launch the web UI bridge and open the browser.
async fn run_web(_args: &[String]) -> std::io::Result<()> {
    // Find bridge.mjs: CWD first, then walk up from the binary's directory.
    let bridge_path = {
        let cwd = std::env::current_dir()?;
        if cwd.join("webui/bridge.mjs").exists() {
            cwd.join("webui/bridge.mjs")
        } else {
            let mut dir = std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|p| p.to_path_buf()));
            let mut found = None;
            for _ in 0..5 {
                if let Some(ref d) = dir {
                    if d.join("webui/bridge.mjs").exists() {
                        found = Some(d.join("webui/bridge.mjs"));
                        break;
                    }
                    dir = d.parent().map(|p| p.to_path_buf());
                }
            }
            found.ok_or_else(|| {
                std::io::Error::other(
                    "webui/bridge.mjs not found — place it in ./webui/ or install bone via git/npm",
                )
            })?
        }
    };

    // Check node is available
    if std::process::Command::new("node")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_err()
    {
        return Err(std::io::Error::other(
            "node not found — install Node.js to use the web UI",
        ));
    }

    let mut command = tokio::process::Command::new("node");
    command.arg(&bridge_path);
    if std::env::var_os("BONE_BIN").is_none() {
        if let Ok(exe) = std::env::current_exe() {
            command.env("BONE_BIN", exe);
        }
    }

    let mut child = command
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .spawn()?;

    child.wait().await?;
    Ok(())
}

/// `bone connect` — a line-oriented RPC frontend, the reference *remote client*:
/// each stdin line is a prompt, the daemon's `RuntimeEvent`s are printed, and
/// tool-approval requests are answered over the wire (auto-approve what the
/// approval mode already allows, otherwise deny). Drives the runtime purely
/// through [`SocketConn`] — the same transport a remote TUI would use.
async fn run_connect(args: &[String]) -> std::io::Result<()> {
    use bone::runtime::{RuntimeCommand, RuntimeConn, RuntimeEvent, SocketConn};
    use bone::tools::CallOutcome;
    use tokio::io::AsyncBufReadExt;

    let addr = parse_listen_addr(args);
    let stream = tokio::net::TcpStream::connect(&addr).await?;
    let (read_half, write_half) = tokio::io::split(stream);
    let mut conn = SocketConn::new(read_half, write_half);
    // Cloned handle so the stdin/approval arms can queue commands without
    // borrowing `conn`, which `next_event` holds mutably in the other arm.
    let commands = conn.command_sender();

    eprintln!(
        "bone: connected to {addr}; type a prompt and press enter. (Ctrl+C cancels a turn; Ctrl+D quits)"
    );
    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    loop {
        tokio::select! {
            // Intercept SIGINT so Ctrl+C cancels the in-flight turn instead
            // of killing the client mid-stream. Re-armed each iteration.
            // Quit with Ctrl+D (EOF) or by closing stdin.
            _ = tokio::signal::ctrl_c() => {
                let _ = commands.send(RuntimeCommand::Cancel);
                eprintln!("bone: cancelled current turn");
            },
            line = lines.next_line() => match line? {
                Some(line) if !line.trim().is_empty() => {
                    let _ = commands.send(RuntimeCommand::SubmitPrompt { text: line, images: vec![] });
                }
                Some(_) => {}
                None => break, // stdin closed
            },
            ev = conn.next_event() => match ev {
                Some(RuntimeEvent::ApprovalRequest { id, name, summary, auto_allows, .. }) => {
                    let outcome = if auto_allows {
                        CallOutcome::Approve
                    } else {
                        CallOutcome::Denied
                    };
                    eprintln!(
                        "[approval] {name}: {summary} -> {}",
                        if auto_allows { "approve" } else { "deny (run `bone` for interactive approval)" }
                    );
                    let _ = commands.send(RuntimeCommand::ApprovalReply { id, outcome });
                }
                Some(ev) => println!("{ev:?}"),
                None => {
                    eprintln!("bone: server closed connection");
                    break;
                }
            },
        }
    }
    Ok(())
}
/// Pick a suitable install directory for the bone symlink.
/// Prefers a user-local bin that is already on PATH; falls back to standard locations.
fn install_dir() -> Result<std::path::PathBuf, String> {
    let candidates = if cfg!(windows) {
        // Windows: use the user's local AppData bin
        let appdata = std::env::var("LOCALAPPDATA").unwrap_or_default();
        vec![
            format!("{appdata}\\Microsoft\\WindowsApps"),
            format!("{appdata}\\Programs\\bone"),
        ]
    } else {
        // Unix (Linux, macOS, Termux, etc.)
        let home = std::env::var("HOME").unwrap_or_default();
        vec![
            format!("{home}/.local/bin"),
            "/usr/local/bin".to_string(),
            format!("{home}/bin"),
        ]
    };

    // Pick the first candidate that exists (or that we can create)
    for dir in &candidates {
        let path = std::path::Path::new(dir);
        if path.exists() {
            return Ok(path.to_path_buf());
        }
        // Try to create it
        if std::fs::create_dir_all(path).is_ok() {
            return Ok(path.to_path_buf());
        }
    }
    Err("could not find or create a suitable install directory".to_string())
}

fn do_install() -> std::io::Result<()> {
    let exe = std::env::current_exe().map_err(|e| {
        std::io::Error::other(format!("cannot determine current executable path: {e}"))
    })?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);

    let dir = install_dir().map_err(std::io::Error::other)?;
    let link = dir.join("bone");

    // Remove old symlink/file if it exists
    let _ = std::fs::remove_file(&link);

    #[cfg(windows)]
    {
        // On Windows, copy the binary instead of symlinking (symlinks need admin)
        std::fs::copy(&exe, &link)?;
    }
    #[cfg(not(windows))]
    {
        std::os::unix::fs::symlink(&exe, &link)?;
    }

    println!("Installed: {} -> {}", link.display(), exe.display());

    // Warn if the install dir is not on PATH
    let path_var = std::env::var("PATH").unwrap_or_default();
    let dir_str = dir.to_string_lossy();
    let on_path = if cfg!(windows) {
        // Windows PATH uses ; separator and is case-insensitive
        path_var
            .split(';')
            .any(|p| p.eq_ignore_ascii_case(&dir_str))
    } else {
        path_var.split(':').any(|p| p == dir_str)
    };

    if !on_path {
        eprintln!("\nWarning: {} is not on your PATH.", dir.display());
        eprintln!("Add it with:");
        let profile = if cfg!(target_os = "macos") {
            "~/.zprofile"
        } else {
            "~/.profile"
        };
        eprintln!(
            "  echo 'export PATH=\"{}:$PATH\"' >> {}",
            dir.display(),
            profile
        );
        eprintln!("  source {}", profile);
    }

    std::process::exit(0)
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // Shared bootstrap for all entry points
    ensure_deps();

    // `bone setup` — explicit (re)run of the onboarding wizard.
    if args.first().map(String::as_str) == Some("setup") {
        bone::config::seed_base();
        // Exit 2 on cancel so a parent (e.g. the tmux `/setup` popup) can tell
        // a cancelled wizard from an applied one.
        let applied = bone::ui::setup::run(false)?;
        std::process::exit(if applied { 0 } else { 2 });
    }

    // `bone catalog` — browse/install/remove optional tools & commands. Used
    // both directly and by the `/catalog` tmux popup.
    if matches!(args.first().map(String::as_str), Some("catalog")) {
        bone::config::seed_base();
        let outcome = bone::ui::catalog::run()?;
        std::process::exit(if outcome.changed { 0 } else { 2 });
    }

    // `bone update` — check and apply self-updates for npm/git installs. Used
    // both directly and by the `/update` tmux popup.
    if args.first().map(String::as_str) == Some("update") {
        match bone::update_check::run_interactive_update(has_flag(&args[1..], "--yes")) {
            Ok(changed) => std::process::exit(if changed { 0 } else { 2 }),
            Err(err) => {
                eprintln!("Update failed: {err}");
                std::process::exit(1);
            }
        }
    }

    // Headless / non-interactive entry points must never block on the wizard;
    // they seed everything (or honor a prior selection) and proceed.
    let interactive = !matches!(
        args.first().map(String::as_str),
        Some("run")
            | Some("serve")
            | Some("connect")
            | Some("stats-popup")
            | Some("update")
            | Some("install")
            | Some("web")
    );
    if interactive && bone::config::needs_onboarding() {
        // Fresh install: seed the always-safe base so the wizard can `require`
        // its libs, then run onboarding (writes the selection + init.lua).
        bone::config::seed_base();
        bone::ui::setup::run(true)?;
    }
    // Seed tools (and base) filtered by whatever selection is now persisted;
    // None ⇒ seed everything (default / upgrade behavior).
    bone::config::seed_all_with_persisted();

    if args.first().map(String::as_str) == Some("run") {
        let request = run::parse_run_args(&args[1..]).map_err(std::io::Error::other)?;
        let response = run::run_headless(request)
            .await
            .map_err(std::io::Error::other)?;
        println!("{}", response.content);
        return Ok(());
    }
    // Daemon mode: run the runtime headless and accept frontend clients over
    // the RPC protocol (the `nvim --embed`/`--listen` model). `bone connect`
    // is a minimal line-oriented client that exercises it.
    if args.first().map(String::as_str) == Some("serve") {
        return run_serve(&args[1..]).await;
    }
    if args.first().map(String::as_str) == Some("connect") {
        return run_connect(&args[1..]).await;
    }
    if args.first().map(String::as_str) == Some("stats-popup") {
        let db = bone::session_db::SessionDb::open(&bone::session_db::db_path())
            .map_err(std::io::Error::other)?;
        bone::ui::stats::run(|range| match range {
            None => db
                .usage_stats_snapshot()
                .map_err(|err| std::io::Error::other(err.to_string())),
            Some(r) => db
                .usage_stats_range(&r.start, &r.end)
                .map_err(|err| std::io::Error::other(err.to_string())),
        })?;
        return Ok(());
    }
    // Install: symlink bone binary into PATH
    if args.first().map(String::as_str) == Some("install") {
        return do_install();
    }

    // `bone web` — launch the web UI bridge (http://localhost:4577).
    if args.first().map(String::as_str) == Some("web") {
        return run_web(&args[1..]).await;
    }

    // Throttled, non-blocking catalog refresh: pulls the latest index on a
    // background thread so update flags / the startup hint stay current.
    // Installs nothing — updates are applied only via `/catalog`.
    bone::ext::catalog::refresh_in_background();
    // Throttled, non-blocking self-update check: fetches the latest release
    // for this install source; the banner surfaces it next launch if newer.
    bone::update_check::check_in_background();

    // Normal TUI mode
    let mut custom = CustomConfigs::load();
    let cfg = UserConfig::from_custom_configs(&custom);
    let mut providers_config = custom.derive_providers_config();

    let cli_options = parse_cli_options(&args).map_err(std::io::Error::other)?;
    let provider_id = cli_options.provider.unwrap_or_else(|| {
        if custom.get_last_provider().is_empty() {
            "local".to_string()
        } else {
            custom.get_last_provider()
        }
    });

    if let Some(model) = cli_options.model.as_ref() {
        if let Some(entry) = custom.get_provider_entry("providers", &provider_id) {
            let mut entry = entry;
            entry.model = model.clone();
            custom.set_provider_entry("providers", &provider_id, &entry);
            // Update the derived config too
            providers_config = custom.derive_providers_config();
        } else {
            return Err(std::io::Error::other(format!(
                "unknown provider `{provider_id}`"
            )));
        }
    }
    if cli_options.changed {
        custom.set_last_provider(&provider_id);
        providers_config.last_provider = provider_id.clone();
    }
    bone::config::warn_if_no_api_key_for(&provider_id, &providers_config);

    let provider = providers::create_provider_with_config(&provider_id, &providers_config)
        .map_err(std::io::Error::other)?;

    // Remote mode: attach the full TUI to a separate `bone serve` daemon. The
    // daemon owns the session and runs turns; the local provider is constructed
    // only for initial display strings (the daemon's StateSnapshot corrects
    // them), so its credentials aren't validated here.
    if let Some(addr) = cli_options.connect {
        let stream = tokio::net::TcpStream::connect(&addr).await.map_err(|e| {
            std::io::Error::other(format!("failed to connect to daemon at {addr}: {e}"))
        })?;
        let (read_half, write_half) = tokio::io::split(stream);
        let client = bone::rpc::RemoteClient::connect(read_half, write_half);
        let mut app = App::with_daemon(provider, cfg, custom, client)?;
        app.run().await?;
        return Ok(());
    }

    // Fail fast on bad credentials before booting Lua or the TUI.
    provider.validate().await.map_err(std::io::Error::other)?;

    // Default: in-process runtime host. The runtime (daemon) runs on a
    // LocalSet alongside the TUI — one process, no socket, no child spawn.
    // The TUI is a pure client pushing RuntimeCommands and rendering
    // RuntimeEvents over in-process channels.
    let provider: std::sync::Arc<dyn LlmProvider> = std::sync::Arc::from(provider);
    // The interactive TUI starts a fresh conversation each launch (clean slate);
    // past chats remain in the DB and are reachable via /history. Only the
    // multi-chat `bone serve` / web UI resumes the latest conversation on attach.
    let boot = boot_runtime_host_for(provider, &mut custom, bone::rpc::SessionTarget::New)?;

    let (hub, commands_rx) = bone::rpc::Hub::new();
    let command_tx = hub.command_sender();
    let events_rx = hub.subscribe();

    // Publish boot-time frontend/session state so the pure-client TUI renders
    // the runtime's theme/keymap/banner/commands/tools and current conversation
    // id immediately.
    {
        let s = boot.session.lock().unwrap();
        hub.publish(bone::rpc::frontend_state(&boot.manager, &s.tools));
        hub.publish(bone::runtime::RuntimeEvent::StateSnapshot {
            snapshot: s.snapshot(boot.provider.id(), boot.provider.model()),
        });
    }

    let mut app = App::with_runtime_client(
        boot.provider.clone(),
        cfg,
        custom,
        command_tx,
        events_rx,
        None,
    )?;

    // run_daemon is !Send (owns the Lua VM via session), so drive it and
    // the TUI together on a LocalSet.
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let daemon = tokio::task::spawn_local(bone::rpc::run_daemon(
                hub.publisher(),
                commands_rx,
                boot.provider,
                boot.manager,
                boot.session,
                bone::tools::ApprovalMode::Safe,
                None,
                true, // forward view diffs: the TUI is a pure client
                // The in-process TUI drains background jobs / inbox itself
                // (`tick_jobs` / `tick_inbox`), so the daemon must not also.
                false,
            ));
            let result = app.run().await;
            daemon.abort();
            result
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::{has_flag, parse_provider_model};

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_provider_model_extracts_both() {
        let (p, m) = parse_provider_model(&args(&[
            "--listen",
            "127.0.0.1:7878",
            "--provider",
            "codex",
            "--model",
            "gpt-5.5",
        ]));
        assert_eq!(p.as_deref(), Some("codex"));
        assert_eq!(m.as_deref(), Some("gpt-5.5"));
    }

    #[test]
    fn parse_provider_model_ignores_unknown_and_missing() {
        // Unlike `parse_cli_options`, unknown flags (e.g. `--listen`) are ignored
        // rather than rejected, and absent flags yield `None`.
        let (p, m) = parse_provider_model(&args(&["--listen", "x", "--verbose"]));
        assert!(p.is_none());
        assert!(m.is_none());
    }

    #[test]
    fn has_flag_detects_presence() {
        assert!(has_flag(
            &args(&["--shutdown-on-stdin-eof"]),
            "--shutdown-on-stdin-eof"
        ));
        assert!(!has_flag(
            &args(&["--listen", "x"]),
            "--shutdown-on-stdin-eof"
        ));
    }
}

use bone::config::{UserConfig, custom::CustomConfigs};
use bone::llm::providers;
use bone::run;
use bone::ui::app::App;
struct CliOptions {
    provider: Option<String>,
    model: Option<String>,
    changed: bool,
}

fn parse_cli_options(args: &[String]) -> Result<CliOptions, String> {
    let mut provider = None;
    let mut model = None;

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
            "--help" | "-h" => return Err(usage()),
            other => return Err(format!("unknown argument: {other}\n{}", usage())),
        }
        i += 1;
    }

    let changed = provider.is_some() || model.is_some();
    Ok(CliOptions {
        provider,
        model,
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
        label: "uv (needed by web_search, task_list, cron)",
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
    "Usage: bone [--provider <id>] [--model <name>]\n       bone agent [--provider <id>] [--model <name>] ...\n       bone serve [--listen <addr>]   # run as a daemon (default 127.0.0.1:7878)\n       bone connect [--listen <addr>] # line-oriented RPC client".to_string()
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

/// `bone serve` — bind a socket, run the headless runtime, and fan its events
/// out to every attached frontend. Each `SubmitPrompt` drives a full agent turn
/// whose events stream back over the protocol.
async fn run_serve(args: &[String]) -> std::io::Result<()> {
    let addr = parse_listen_addr(args);

    // Build the provider from config, exactly like TUI mode.
    let custom = CustomConfigs::load();
    let providers_config = custom.derive_providers_config();
    let provider_id = if custom.get_last_provider().is_empty() {
        "local".to_string()
    } else {
        custom.get_last_provider()
    };
    let provider = providers::create_provider_with_config(&provider_id, &providers_config)
        .map_err(std::io::Error::other)?;
    let provider: std::sync::Arc<dyn bone::llm::provider::LlmProvider> =
        std::sync::Arc::from(provider);

    let (hub, commands_rx) = bone::rpc::Hub::new();
    tokio::spawn(bone::rpc::run_daemon(
        hub.clone(),
        commands_rx,
        Some(provider),
        bone::tools::ApprovalMode::Safe,
    ));

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    eprintln!("bone: serving runtime on {addr} (provider: {provider_id})");
    loop {
        let (stream, peer) = listener.accept().await?;
        eprintln!("bone: client attached: {peer}");
        let hub = hub.clone();
        tokio::spawn(async move {
            if let Err(e) = bone::rpc::serve_connection(stream, hub, Vec::new()).await {
                eprintln!("bone: client {peer} ended: {e}");
            }
        });
    }
}

/// `bone connect` — a minimal RPC frontend: each stdin line is a prompt; every
/// `RuntimeEvent` from the daemon is printed. Proves the protocol end to end.
async fn run_connect(args: &[String]) -> std::io::Result<()> {
    use tokio::io::AsyncBufReadExt;

    let addr = parse_listen_addr(args);
    let stream = tokio::net::TcpStream::connect(&addr).await?;
    let (read_half, mut write_half) = tokio::io::split(stream);

    tokio::spawn(async move {
        let mut reader = bone::rpc::codec::MessageReader::new(read_half);
        while let Some(result) = reader.read::<bone::runtime::RuntimeEvent>().await {
            match result {
                Ok(ev) => println!("{ev:?}"),
                Err(_) => continue,
            }
        }
        eprintln!("bone: server closed connection");
    });

    eprintln!("bone: connected to {addr}; type a prompt and press enter.");
    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        bone::rpc::codec::write_message(
            &mut write_half,
            &bone::runtime::RuntimeCommand::SubmitPrompt { text: line },
        )
        .await?;
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
    bone::config::seed_all();

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
        bone::ui::stats::run(|| {
            db.usage_stats_snapshot()
                .map_err(|err| std::io::Error::other(err.to_string()))
        })?;
        return Ok(());
    }
    // Install: symlink bone binary into PATH
    if args.first().map(String::as_str) == Some("install") {
        return do_install();
    }

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
    provider.validate().await.map_err(std::io::Error::other)?;

    let mut app = App::new(provider, cfg, custom)?;
    app.run().await?;
    Ok(())
}

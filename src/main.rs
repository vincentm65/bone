use bone::agent;
use bone::config::{UserConfig, custom::CustomConfigs, load_providers, save_providers};
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

fn ensure_uv() {
    let found = std::process::Command::new("uv")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok();
    if !found {
        eprintln!("bone: 'uv' is required but not found on PATH.");
        eprintln!();
        eprintln!("Install uv:");
        eprintln!("  macOS/Linux:  curl -LsSf https://astral.sh/uv/install.sh | sh");
        eprintln!("  macOS (brew): brew install uv");
        eprintln!(
            "  Windows:      powershell -ExecutionPolicy ByPass -c \"irm https://astral.sh/uv/install.ps1 | iex\""
        );
        eprintln!("  pip:          pip install uv");
        eprintln!("  See: https://docs.astral.sh/uv/getting-started/installation/");
        eprintln!();
        eprintln!("Once installed, uv will auto-provision Python when needed.");
        std::process::exit(1);
    }
}

fn usage() -> String {
    "Usage: bone [--provider <id>] [--model <name>]\n       bone agent [--provider <id>] [--model <name>] ...".to_string()
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
    ensure_uv();
    bone::config::seed_all();

    if args.first().map(String::as_str) == Some("run") {
        let request = run::parse_run_args(&args[1..]).map_err(std::io::Error::other)?;
        let response = run::run_headless(request)
            .await
            .map_err(std::io::Error::other)?;
        println!("{}", response.content);
        return Ok(());
    }
    // Install: symlink bone binary into PATH
    if args.first().map(String::as_str) == Some("install") {
        return do_install();
    }

    // Dispatch headless sub-agent mode
    if args.first().map(String::as_str) == Some("agent") {
        let request = agent::parse_agent_args(&args[1..]).map_err(std::io::Error::other)?;
        if request.events {
            // Events mode: JSONL is streamed to stdout by the agent loop
            agent::run_agent(request)
                .await
                .map_err(std::io::Error::other)?;
        } else {
            // Plain mode: print final answer
            let response = agent::run_agent(request)
                .await
                .map_err(std::io::Error::other)?;
            println!("{}", response.content);
        }
        return Ok(());
    }

    // Normal TUI mode
    let custom = CustomConfigs::load();
    let cfg = UserConfig::from_custom_configs(&custom);
    let mut providers_config = load_providers();

    let cli_options = parse_cli_options(&args).map_err(std::io::Error::other)?;
    let provider_id = cli_options.provider.unwrap_or_else(|| {
        if providers_config.last_provider.is_empty() {
            "local".to_string()
        } else {
            providers_config.last_provider.clone()
        }
    });

    if let Some(model) = cli_options.model.as_ref() {
        let entry = providers_config
            .providers
            .get_mut(&provider_id)
            .ok_or_else(|| std::io::Error::other(format!("unknown provider `{provider_id}`")))?;
        entry.model = model.clone();
    }
    if cli_options.changed {
        providers_config.last_provider = provider_id.clone();
        save_providers(&providers_config);
    }
    bone::config::warn_if_no_api_key_for(&provider_id, &providers_config);

    let provider = providers::create_provider_with_config(&provider_id, &providers_config)
        .map_err(std::io::Error::other)?;
    provider.validate().await.map_err(std::io::Error::other)?;

    let mut app = App::new(provider, providers_config, cfg, custom)?;
    app.run().await?;
    Ok(())
}

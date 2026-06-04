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

/// Deps that bone tools/skills need at runtime. None are required for the base app.
struct Dep {
    bin: &'static str,
    /// Package name if different from binary name. None = same as bin.
    pkg: Option<&'static str>,
    label: &'static str,
}

const DEPS: &[Dep] = &[
    Dep { bin: "uv", pkg: None, label: "uv (needed by web_search, task_list, subagent, cron)" },
    Dep { bin: "git", pkg: None, label: "git (needed by /r skill and git workflow)" },
    Dep { bin: "sqlite3", pkg: Some("sqlite3"), label: "sqlite3 (needed by /memory skill)" },
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
        return run_silent("winget", &["install", "--id", winget_id, "-e", "--source", "winget", "--accept-package-agreements"]);
    }

    // Fallback for uv: official installer
    if bin == "uv" {
        if cfg!(windows) {
            return run_silent("powershell", &["-ExecutionPolicy", "ByPass", "-c", "irm https://astral.sh/uv/install.ps1 | iex"]);
        }
        return run_silent("sh", &["-c", "curl -LsSf https://astral.sh/uv/install.sh | sh"]);
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
        eprintln!("The base app works without these. They're only needed for the tools listed above.");
    }

    let _ = std::fs::write(&marker, "");
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

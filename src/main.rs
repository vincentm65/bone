use bone::agent;
use bone::config::{
    load_providers, load_user_config, save_providers, seed_command_policy_if_missing,
    seed_providers_if_missing,
};
use bone::cron;
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

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.first().map(String::as_str) == Some("run") {
        ensure_uv();
        let request = run::parse_run_args(&args[1..]).map_err(std::io::Error::other)?;
        let response = run::run_headless(request)
            .await
            .map_err(std::io::Error::other)?;
        println!("{}", response.content);
        return Ok(());
    }

    if args.first().map(String::as_str) == Some("cron") {
        cron::handle_cron_args(&args[1..]).map_err(std::io::Error::other)?;
        return Ok(());
    }

    // Dispatch headless sub-agent mode
    if args.first().map(String::as_str) == Some("agent") {
        ensure_uv();
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
    // Check for uv dependency (required for Python-based tools)
    ensure_uv();

    seed_providers_if_missing();
    seed_command_policy_if_missing();

    let cfg = load_user_config();
    let mut providers_config = load_providers();

    let cli_options = parse_cli_options(&args).map_err(std::io::Error::other)?;
    let provider_id = cli_options.provider.unwrap_or_else(|| {
        if providers_config.last_provider.is_empty() {
            cfg.provider.clone()
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

    let provider = providers::create_provider_with_config(&provider_id, &providers_config)
        .map_err(std::io::Error::other)?;
    provider.validate().await.map_err(std::io::Error::other)?;

    let mut app = App::new(provider, providers_config, cfg)?;
    app.run().await?;
    Ok(())
}

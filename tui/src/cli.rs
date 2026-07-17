//! CLI option parsing and usage text for the bone binary.

use bone_core::tools::ApprovalMode;

pub struct CliOptions {
    pub provider: Option<String>,
    pub model: Option<String>,
    /// `--connect <addr>`: run the full TUI against a remote `bone serve`
    /// daemon instead of an in-process one.
    pub connect: Option<String>,
    pub changed: bool,
}

pub fn parse_cli_options(args: &[String]) -> Result<CliOptions, String> {
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

pub fn usage() -> String {
    "Usage: bone [--provider <id>] [--model <name>]
       bone --connect <addr>          # TUI against remote `bone serve`
       bone run [--provider <id>] [--model <name>] ...
       bone serve [--listen <addr>]   # multi-client daemon (default 127.0.0.1:7878)
       bone connect [--listen <addr>] # line-oriented RPC client
       bone web                       # web UI bridge (http://localhost:4577)
       bone setup | catalog | update | install | stats-popup"
        .to_string()
}

/// Parse `--listen <addr>` from args, falling back to the default.
pub fn parse_listen_addr(args: &[String]) -> String {
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
pub fn parse_provider_model(args: &[String]) -> (Option<String>, Option<String>) {
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

pub fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

pub fn approval_mode(value: &str) -> ApprovalMode {
    ApprovalMode::parse_lenient(value)
}

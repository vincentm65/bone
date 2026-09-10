//! Command-line parsing for the desktop frontend.
//!
//! The desktop app is a pure client of a `bone serve` daemon, so its CLI is a
//! strict subset of the `bone` binary's: startup options plus a few subcommands
//! that merely open a dialog. Daemon-side subcommands (`serve`, `web`, `run`,
//! `install`, `update`) are rejected with a pointer to `bone`.

use crate::daemon;

/// What the process should do after parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Command {
    /// Launch the GUI.
    #[default]
    Gui,
    /// Print the version and exit.
    Version,
    /// Print usage and exit.
    Help,
    /// A subcommand owned by the daemon/core `bone` binary, not this client.
    DaemonSide(&'static str),
}

/// Parsed command line for one process launch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cli {
    /// `--connect <addr>` / `connect --listen <addr>`: daemon address to open
    /// tabs against (loopback only; remote access must tunnel to loopback).
    pub address: Option<String>,
    /// `--provider <id>`: activate this provider once connected.
    pub provider: Option<String>,
    /// `--model <name>`: set the active provider's model once connected.
    pub model: Option<String>,
    /// Open the provider setup wizard on launch.
    pub open_setup: bool,
    /// Open the catalog browser on launch.
    pub open_catalog: bool,
    /// Open the token-stats dashboard on launch.
    pub open_stats: bool,
    pub command: Command,
}

/// Parse process arguments (without the program name). Returns a human-readable
/// message on error; the caller prints it and exits non-zero.
pub fn parse(args: &[String]) -> Result<Cli, String> {
    let mut cli = Cli::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => cli.command = Command::Help,
            "-V" | "--version" => cli.command = Command::Version,
            "--connect" | "--listen" => {
                let value = args.get(i + 1).ok_or("--connect requires an address")?;
                cli.address = Some(validate_address(value)?);
                i += 1;
            }
            "--provider" => {
                let value = args.get(i + 1).ok_or("--provider requires a value")?;
                cli.provider = Some(value.clone());
                i += 1;
            }
            "--model" => {
                let value = args.get(i + 1).ok_or("--model requires a value")?;
                cli.model = Some(value.clone());
                i += 1;
            }
            // Dialog-opening subcommands mirror the TUI's `/setup`, `/catalog`
            // and `stats-popup`.
            "setup" => cli.open_setup = true,
            "catalog" => cli.open_catalog = true,
            "stats-popup" => cli.open_stats = true,
            // Daemon-side operations: this binary has no runtime to run them.
            "connect" => cli.command = Command::DaemonSide("connect"),
            "serve" => cli.command = Command::DaemonSide("serve"),
            "web" => cli.command = Command::DaemonSide("web"),
            "run" => cli.command = Command::DaemonSide("run"),
            "install" => cli.command = Command::DaemonSide("install"),
            "update" => cli.command = Command::DaemonSide("update"),
            other => return Err(format!("unknown argument: {other}\n\n{}", usage())),
        }
        i += 1;
    }
    Ok(cli)
}

/// Normalize a connect address and reject non-loopback targets early with the
/// same explanation the runtime uses. Remote access must go through an SSH
/// tunnel terminating on loopback.
fn validate_address(value: &str) -> Result<String, String> {
    let address = daemon::ensure_port(value);
    daemon::local_endpoints(&address).map_err(|_| {
        "Direct remote connections are disabled: Bone TCP has no encryption or authentication. \
         Use an SSH tunnel and connect to 127.0.0.1:<forwarded-port>."
            .to_string()
    })?;
    Ok(address)
}

/// Version line, matching the `bone` binary's `--version` intent.
pub fn version() -> String {
    format!("bone-desktop {}", env!("CARGO_PKG_VERSION"))
}

pub fn usage() -> String {
    "Usage: bone-desktop [--connect <addr>] [--provider <id>] [--model <name>]
       bone-desktop setup          # open the provider setup wizard
       bone-desktop catalog        # open the catalog browser
       bone-desktop stats-popup    # open the token-stats dashboard
       bone-desktop connect --listen <addr>   # GUI against a daemon

Options:
  --connect <addr>   Daemon address (default 127.0.0.1:7878; loopback only)
  --provider <id>    Activate this provider once connected
  --model <name>     Set the active provider's model once connected
  -V, --version      Print version and exit
  -h, --help         Print this help and exit

bone-desktop is a pure client. Daemon-side commands (serve, web, run, install,
update) live in the `bone` binary."
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_string()).collect()
    }

    #[test]
    fn defaults_to_gui_with_no_flags() {
        let cli = parse(&[]).unwrap();
        assert_eq!(cli, Cli::default());
        assert_eq!(cli.command, Command::Gui);
    }

    #[test]
    fn version_and_help_short_and_long() {
        assert_eq!(parse(&args(&["-V"])).unwrap().command, Command::Version);
        assert_eq!(
            parse(&args(&["--version"])).unwrap().command,
            Command::Version
        );
        assert_eq!(parse(&args(&["-h"])).unwrap().command, Command::Help);
        assert_eq!(parse(&args(&["--help"])).unwrap().command, Command::Help);
    }

    #[test]
    fn connect_flag_normalizes_port_and_accepts_loopback() {
        let cli = parse(&args(&["--connect", "127.0.0.1:9000"])).unwrap();
        assert_eq!(cli.address.as_deref(), Some("127.0.0.1:9000"));
        let cli = parse(&args(&["--connect", "localhost"])).unwrap();
        assert_eq!(cli.address.as_deref(), Some("localhost:7878"));
    }

    #[test]
    fn connect_flag_rejects_non_loopback_and_missing_value() {
        let error = parse(&args(&["--connect", "10.0.0.5:7878"])).unwrap_err();
        assert!(error.contains("SSH tunnel"), "unexpected: {error}");
        assert!(parse(&args(&["--connect"])).is_err());
    }

    #[test]
    fn provider_and_model_flags_capture_values() {
        let cli = parse(&args(&["--provider", "openai", "--model", "gpt-5"])).unwrap();
        assert_eq!(cli.provider.as_deref(), Some("openai"));
        assert_eq!(cli.model.as_deref(), Some("gpt-5"));
        assert_eq!(cli.command, Command::Gui);
        assert!(parse(&args(&["--provider"])).is_err());
        assert!(parse(&args(&["--model"])).is_err());
    }

    #[test]
    fn dialog_subcommands_set_open_flags() {
        assert!(parse(&args(&["setup"])).unwrap().open_setup);
        assert!(parse(&args(&["catalog"])).unwrap().open_catalog);
        assert!(parse(&args(&["stats-popup"])).unwrap().open_stats);
    }

    #[test]
    fn daemon_side_subcommands_are_named_not_run() {
        for name in ["serve", "web", "run", "install", "update", "connect"] {
            let cli = parse(&args(&[name])).unwrap();
            assert_eq!(cli.command, Command::DaemonSide(name));
        }
    }

    #[test]
    fn unknown_argument_is_an_error_with_usage() {
        let error = parse(&args(&["--nope"])).unwrap_err();
        assert!(error.contains("unknown argument: --nope"));
        assert!(error.contains("Usage: bone-desktop"));
    }

    #[test]
    fn version_reports_the_crate_version() {
        assert_eq!(
            version(),
            format!("bone-desktop {}", env!("CARGO_PKG_VERSION"))
        );
    }
}

//! Command-line parsing for the desktop frontend.
//!
//! The desktop app is a pure client of a `bone serve` daemon, so its CLI is a
//! strict subset of the `bone` binary's: startup options only. Daemon-side
//! subcommands (`serve`, `web`, `run`,
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
    /// `--ssh <host>`: reach the daemon via `ssh <host> -- bone stdio`.
    pub ssh_host: Option<String>,
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
            "--ssh" => {
                let value = args.get(i + 1).ok_or("--ssh requires a host")?;
                bone_client::ssh::ssh_args(value, "bone").map_err(|error| error.to_string())?;
                cli.ssh_host = Some(value.trim().to_string());
                i += 1;
            }
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
    if cli.address.is_some() && cli.ssh_host.is_some() {
        return Err("--connect and --ssh are mutually exclusive".into());
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
    "Usage: bone-desktop [--connect <addr> | --ssh <host>]

Options:
  --connect <addr>   Daemon address (default 127.0.0.1:7878; loopback only)
  --ssh <host>       Use the daemon on <host> via `ssh <host> -- bone stdio`
                     (keys or an agent required; `bone` must be on its PATH)
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
    fn ssh_flag_takes_a_host_and_rejects_option_like_values() {
        let cli = parse(&args(&["--ssh", "devbox"])).unwrap();
        assert_eq!(cli.ssh_host.as_deref(), Some("devbox"));
        assert!(parse(&args(&["--ssh"])).is_err());
        assert!(parse(&args(&["--ssh", "-oProxyCommand=x"])).is_err());
        assert!(parse(&args(&["--ssh", "devbox", "--connect", "127.0.0.1"])).is_err());
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

//! Throttled, non-blocking check for newer app releases.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const THROTTLE: Duration = Duration::from_secs(24 * 3600);
const TIMEOUT: Duration = Duration::from_secs(8);
const NPM_URL: &str = "https://registry.npmjs.org/bone-agent/latest";

#[derive(Clone, Debug, PartialEq, Eq)]
enum InstallKind {
    Npm,
    Git(PathBuf),
    Cargo(Option<PathBuf>),
    Unknown,
}

impl InstallKind {
    fn key(&self) -> &'static str {
        match self {
            Self::Npm => "npm",
            Self::Git(_) => "git",
            Self::Cargo(_) => "cargo",
            Self::Unknown => "unknown",
        }
    }

    fn description(&self) -> String {
        match self {
            Self::Npm => "npm".into(),
            Self::Git(root) => format!("git ({})", root.display()),
            Self::Cargo(Some(root)) => format!("cargo ({})", root.display()),
            Self::Cargo(None) => "cargo".into(),
            Self::Unknown => "unknown".into(),
        }
    }

    fn can_apply(&self) -> bool {
        match self {
            Self::Npm | Self::Git(_) => true,
            // cargo install writes the executable currently running from
            // CARGO_HOME/bin; Windows locks it until this process exits.
            Self::Cargo(Some(_)) => !cfg!(windows),
            Self::Cargo(None) | Self::Unknown => false,
        }
    }

    fn notice(&self, latest: &str) -> String {
        if self.can_apply() {
            format!("bone {latest} available — run /update or `bone update`")
        } else {
            format!(
                "bone {latest} available — update with: {}",
                self.update_hint()
            )
        }
    }

    fn update_hint(&self) -> String {
        match self {
            Self::Npm | Self::Git(_) => "bone update".to_string(),
            Self::Cargo(Some(root)) if cfg!(windows) => format!(
                "after exiting bone, run:\n{}",
                source_update_commands(root, true)
            ),
            Self::Cargo(Some(_)) => "bone update".to_string(),
            Self::Cargo(None) => {
                "reinstall from source or run: npm install -g bone-agent@latest".into()
            }
            Self::Unknown => "npm install -g bone-agent@latest".into(),
        }
    }

    fn apply(&self) -> Result<(), String> {
        match self {
            Self::Npm => {
                run_command(Command::new("npm").args(["install", "-g", "bone-agent@latest"]))
            }
            Self::Git(root) | Self::Cargo(Some(root)) => {
                run_command(
                    Command::new("git")
                        .args(["-C"])
                        .arg(root)
                        .args(["pull", "--ff-only"]),
                )?;
                run_command(
                    Command::new("cargo")
                        .current_dir(root)
                        .args(["install", "--path", "tui", "--force"]),
                )
            }
            Self::Cargo(None) | Self::Unknown => {
                Err("this install source can't be updated automatically".to_string())
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionStatus {
    pub current: String,
    pub latest: String,
    pub update_available: bool,
    pub install_source: String,
    pub update_hint: String,
    pub can_apply: bool,
}

fn run_command(cmd: &mut Command) -> Result<(), String> {
    let status = cmd
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|err| format!("failed to run updater: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("updater exited with {status}"))
    }
}

fn cache_dir() -> Option<PathBuf> {
    crate::config::try_bone_dir()
}

fn cache_file(kind: &InstallKind, suffix: &str) -> Option<PathBuf> {
    cache_dir().map(|d| d.join(format!("update_{}_{}", kind.key(), suffix)))
}

fn check_due(kind: &InstallKind) -> bool {
    let last = cache_file(kind, "checked_at")
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0);
    check_due_from(
        latest_seen(kind).as_deref(),
        last,
        env!("CARGO_PKG_VERSION"),
        crate::util::now_secs(),
    )
}

fn check_due_from(latest: Option<&str>, last: u64, current: &str, now: u64) -> bool {
    latest.is_none_or(|latest| !is_newer_version(latest, current))
        || now.saturating_sub(last) >= THROTTLE.as_secs()
}

fn mark_checked(kind: &InstallKind) {
    if let Some(dir) = cache_dir() {
        let _ = std::fs::create_dir_all(&dir);
        if let Some(path) = cache_file(kind, "checked_at") {
            let _ = std::fs::write(path, crate::util::now_secs().to_string());
        }
    }
}

fn write_latest(kind: &InstallKind, version: &str) {
    if let Some(dir) = cache_dir() {
        let _ = std::fs::create_dir_all(&dir);
        if let Some(path) = cache_file(kind, "latest") {
            let _ = std::fs::write(path, version.trim());
        }
    }
}

fn detect_install_kind() -> InstallKind {
    if std::env::var("BONE_INSTALL_KIND").as_deref() == Ok("npm") {
        return InstallKind::Npm;
    }
    let exe = std::env::current_exe().ok();
    let exe = exe
        .as_ref()
        .and_then(|p| std::fs::canonicalize(p).ok())
        .or(exe);
    let Some(exe) = exe.as_deref() else {
        return InstallKind::Unknown;
    };
    let detected = detect_install_kind_from(exe);
    if detected != InstallKind::Unknown {
        return detected;
    }
    if is_cargo_executable(exe) {
        return InstallKind::Cargo(cargo_source_root());
    }
    InstallKind::Unknown
}

fn detect_install_kind_from(exe: &Path) -> InstallKind {
    for dir in exe.ancestors() {
        let package_json = dir.join("package.json");
        if std::fs::read_to_string(&package_json)
            .ok()
            .is_some_and(|s| s.contains("\"name\"") && s.contains("\"bone-agent"))
        {
            return InstallKind::Npm;
        }
        if dir.join(".git").exists() {
            return InstallKind::Git(dir.to_path_buf());
        }
    }
    InstallKind::Unknown
}

fn cargo_home() -> Option<PathBuf> {
    std::env::var_os("CARGO_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".cargo")))
}

fn is_cargo_executable(exe: &Path) -> bool {
    let Some(home) = cargo_home() else {
        return false;
    };
    let bin = home.join("bin");
    exe.parent().is_some_and(|parent| {
        std::fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf())
            == std::fs::canonicalize(&bin).unwrap_or(bin)
    })
}

fn cargo_source_path_from_metadata(content: &str) -> Option<PathBuf> {
    let value: serde_json::Value = serde_json::from_str(content).ok()?;
    let installs = value["installs"].as_object()?;
    installs.keys().find_map(|key| {
        let prefix = key.find("bone ")?;
        if prefix != 0 {
            return None;
        }
        let marker = "(path+";
        let start = key.find(marker)? + marker.len();
        let url = key.get(start..)?.strip_suffix(')')?;
        reqwest::Url::parse(url).ok()?.to_file_path().ok()
    })
}

fn cargo_source_root() -> Option<PathBuf> {
    cargo_source_root_from_home(&cargo_home()?)
}

fn cargo_source_root_from_home(home: &Path) -> Option<PathBuf> {
    let metadata = std::fs::read_to_string(home.join(".crates2.json")).ok()?;
    let source = cargo_source_path_from_metadata(&metadata)?;
    source
        .ancestors()
        .find(|directory| directory.join(".git").exists())
        .map(Path::to_path_buf)
}

/// Fetch the latest published version off the main thread. Safe to call at
/// every interactive startup; the banner reads the cached result next launch.
///
/// If the cached version does not prove this binary is stale, check again even
/// inside the throttle window so a same-day release/tag is detected without
/// clearing cache. Once a newer version is cached, throttle normally.
pub fn check_in_background() {
    let kind = detect_install_kind();
    if !check_due(&kind) {
        return;
    }
    std::thread::spawn(move || {
        if let Ok(version) = fetch_latest() {
            write_latest(&kind, &version);
            mark_checked(&kind);
        }
    });
}

fn fetch_latest() -> Result<String, String> {
    reqwest::blocking::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|error| error.to_string())?
        .get(NPM_URL)
        .header("User-Agent", "bone")
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|error| error.to_string())?
        .json::<serde_json::Value>()
        .map_err(|error| error.to_string())?["version"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| "npm response did not contain a version".to_string())
}

/// Check the installed build against the canonical npm stable release.
pub fn check_now() -> Result<VersionStatus, String> {
    let kind = detect_install_kind();
    let current = env!("CARGO_PKG_VERSION").to_string();
    let latest = fetch_latest()?;
    write_latest(&kind, &latest);
    mark_checked(&kind);
    Ok(VersionStatus {
        update_available: is_newer_version(&latest, &current),
        current,
        latest,
        install_source: kind.description(),
        update_hint: kind.update_hint(),
        can_apply: kind.can_apply(),
    })
}

/// Describe how the current executable was installed.
#[must_use]
pub fn install_source() -> String {
    detect_install_kind().description()
}

/// Interactive updater used by `bone update` and `/update`.
pub fn run_interactive_update(assume_yes: bool) -> Result<bool, String> {
    let kind = detect_install_kind();
    let current = env!("CARGO_PKG_VERSION");
    let latest = fetch_latest().map_err(|error| format!("could not check for updates: {error}"))?;
    write_latest(&kind, &latest);
    mark_checked(&kind);

    if !is_newer_version(&latest, current) {
        println!("bone is up to date ({current}).");
        return Ok(false);
    }

    println!("bone {latest} available (current {current}).");
    if !kind.can_apply() {
        println!("Update with:\n{}", kind.update_hint());
        return Ok(false);
    }
    if !assume_yes {
        print!("Apply update now? [y/N] ");
        io::stdout().flush().map_err(|err| err.to_string())?;
        let mut answer = String::new();
        io::stdin()
            .read_line(&mut answer)
            .map_err(|err| err.to_string())?;
        if !matches!(answer.trim(), "y" | "Y" | "yes" | "YES") {
            return Ok(false);
        }
    }

    kind.apply()?;
    println!("bone updated to {latest}.");
    Ok(true)
}

fn latest_seen(kind: &InstallKind) -> Option<String> {
    let path = cache_file(kind, "latest")?;
    let s = std::fs::read_to_string(path).ok()?;
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// User-facing update notice for the startup banner, if this install is behind.
pub fn notice() -> Option<String> {
    let kind = detect_install_kind();
    let latest = latest_seen(&kind)?;
    is_newer_version(&latest, env!("CARGO_PKG_VERSION")).then(|| kind.notice(&latest))
}

fn is_newer_version(latest: &str, current: &str) -> bool {
    let parse = |version: &str| semver::Version::parse(version.trim_start_matches('v'));
    match (parse(latest), parse(current)) {
        (Ok(latest), Ok(current)) => latest > current,
        _ => false,
    }
}

fn source_update_commands(root: &Path, windows: bool) -> String {
    let tui = if windows {
        PathBuf::from(format!(
            "{}\\tui",
            root.to_string_lossy().trim_end_matches(['\\', '/'])
        ))
    } else {
        root.join("tui")
    };
    format!(
        "git -C {} pull --ff-only\ncargo install --path {} --force",
        command_path(root, windows),
        command_path(&tui, windows)
    )
}

fn command_path(path: &Path, windows: bool) -> String {
    let s = path.to_string_lossy();
    if windows {
        // Double-quoted paths work in cmd.exe and PowerShell. Windows paths
        // cannot contain a literal double quote, so no shell-specific escape
        // sequence is required here.
        format!("\"{s}\"")
    } else if s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-".contains(c))
    {
        s.into_owned()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
#[path = "update_check_tests.rs"]
mod update_check_tests;

//! Throttled, non-blocking check for newer app releases.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const THROTTLE: Duration = Duration::from_secs(24 * 3600);
const TIMEOUT: Duration = Duration::from_secs(8);
const GITHUB_URL: &str = "https://api.github.com/repos/vincentm65/bone/tags";
const NPM_URL: &str = "https://registry.npmjs.org/bone-agent/latest";

#[derive(Clone, Debug, PartialEq, Eq)]
enum InstallKind {
    Npm,
    Git(PathBuf),
    Unknown,
}

impl InstallKind {
    fn key(&self) -> &'static str {
        match self {
            Self::Npm => "npm",
            Self::Git(_) | Self::Unknown => "github",
        }
    }

    fn url(&self) -> &'static str {
        match self {
            Self::Npm => NPM_URL,
            Self::Git(_) | Self::Unknown => GITHUB_URL,
        }
    }

    fn latest_from_json(&self, v: &serde_json::Value) -> Option<String> {
        match self {
            Self::Npm => v["version"].as_str().map(str::to_string),
            Self::Git(_) | Self::Unknown => v
                .as_array()
                .and_then(|tags| {
                    tags.iter()
                        .filter_map(|tag| tag["name"].as_str())
                        .max_by_key(|name| version_key(name))
                })
                .or_else(|| v["tag_name"].as_str())
                .map(|s| s.trim_start_matches('v').to_string()),
        }
    }

    fn notice(&self, latest: &str) -> String {
        match self {
            Self::Unknown => {
                format!("bone {latest} available — https://github.com/vincentm65/bone/releases")
            }
            _ => format!(
                "bone {latest} available — update with: {}",
                self.update_hint()
            ),
        }
    }

    fn update_hint(&self) -> String {
        match self {
            Self::Npm => "npm install -g bone-agent@latest".to_string(),
            Self::Git(root) => format!(
                "cd {} && git pull --ff-only && cargo install --path tui --force",
                shell_quote(root)
            ),
            Self::Unknown => "https://github.com/vincentm65/bone/releases".to_string(),
        }
    }

    fn apply(&self) -> Result<(), String> {
        match self {
            Self::Npm => {
                run_command(Command::new("npm").args(["install", "-g", "bone-agent@latest"]))
            }
            Self::Git(root) => {
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
            Self::Unknown => Err("this install source can't be updated automatically".to_string()),
        }
    }
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

fn cache_dir() -> PathBuf {
    crate::config::bone_dir()
}

fn cache_file(kind: &InstallKind, suffix: &str) -> PathBuf {
    cache_dir().join(format!("update_{}_{}", kind.key(), suffix))
}

fn check_due(kind: &InstallKind) -> bool {
    let last = std::fs::read_to_string(cache_file(kind, "checked_at"))
        .ok()
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
    let _ = std::fs::create_dir_all(cache_dir());
    let _ = std::fs::write(
        cache_file(kind, "checked_at"),
        crate::util::now_secs().to_string(),
    );
}

fn write_latest(kind: &InstallKind, version: &str) {
    let _ = std::fs::create_dir_all(cache_dir());
    let _ = std::fs::write(cache_file(kind, "latest"), version.trim());
}

fn detect_install_kind() -> InstallKind {
    let exe = std::env::current_exe().ok();
    let exe = exe
        .as_ref()
        .and_then(|p| std::fs::canonicalize(p).ok())
        .or(exe);
    exe.as_deref()
        .map(detect_install_kind_from)
        .unwrap_or(InstallKind::Unknown)
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
        if let Some(version) = fetch_latest(&kind) {
            write_latest(&kind, &version);
            mark_checked(&kind);
        }
    });
}

fn fetch_latest(kind: &InstallKind) -> Option<String> {
    reqwest::blocking::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .ok()
        .and_then(|c| c.get(kind.url()).header("User-Agent", "bone").send().ok())
        .and_then(|r| r.json::<serde_json::Value>().ok())
        .and_then(|v| kind.latest_from_json(&v))
}

/// Interactive updater used by `bone update` and `/update`.
pub fn run_interactive_update(assume_yes: bool) -> Result<bool, String> {
    let kind = detect_install_kind();
    let current = env!("CARGO_PKG_VERSION");
    let latest = fetch_latest(&kind).ok_or_else(|| "could not check for updates".to_string())?;
    write_latest(&kind, &latest);
    mark_checked(&kind);

    if !is_newer_version(&latest, current) {
        println!("bone is up to date ({current}).");
        return Ok(false);
    }

    println!("bone {latest} available (current {current}).");
    println!("Update command: {}", kind.update_hint());
    if matches!(kind, InstallKind::Unknown) {
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
    let s = std::fs::read_to_string(cache_file(kind, "latest")).ok()?;
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
    version_key(latest) > version_key(current)
}

fn version_key(s: &str) -> [u64; 3] {
    let mut out = [0, 0, 0];
    for (i, part) in s
        .trim_start_matches('v')
        .split(|c: char| !c.is_ascii_digit())
        .filter(|p| !p.is_empty())
        .take(3)
        .enumerate()
    {
        out[i] = part.parse().unwrap_or(0);
    }
    out
}

fn shell_quote(path: &Path) -> String {
    let s = path.to_string_lossy();
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-".contains(c))
    {
        s.into_owned()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::{InstallKind, THROTTLE, check_due_from, is_newer_version, shell_quote};
    use serde_json::json;
    use std::path::{Path, PathBuf};

    #[test]
    fn compares_versions_as_versions() {
        assert!(is_newer_version("2.10.0", "2.9.9"));
        assert!(is_newer_version("v3.0.0", "2.9.9"));
        assert!(!is_newer_version("2.2.4", "2.2.4"));
        assert!(!is_newer_version("2.2.0", "2.2"));
        assert!(!is_newer_version("2.0.9", "2.2.4"));
    }

    #[test]
    fn quotes_update_paths_for_shell() {
        assert_eq!(shell_quote(Path::new("/tmp/bone")), "/tmp/bone");
        assert_eq!(shell_quote(Path::new("/tmp/my bone")), "'/tmp/my bone'");
    }

    #[test]
    fn reads_latest_from_github_tags_or_release_json() {
        let kind = InstallKind::Git(PathBuf::from("/tmp/bone"));
        assert_eq!(
            kind.latest_from_json(&json!([{ "name": "v2.2.7" }, { "name": "v2.2.8" }]))
                .as_deref(),
            Some("2.2.8")
        );
        assert_eq!(
            kind.latest_from_json(&json!({ "tag_name": "v2.2.9" }))
                .as_deref(),
            Some("2.2.9")
        );
    }

    #[test]
    fn reads_latest_from_npm_json() {
        assert_eq!(
            InstallKind::Npm
                .latest_from_json(&json!({ "version": "2.2.8" }))
                .as_deref(),
            Some("2.2.8")
        );
    }

    #[test]
    fn rechecks_when_cache_does_not_show_stale_binary() {
        let now = THROTTLE.as_secs() + 10_000;
        let recent = now - 10;
        assert!(check_due_from(None, recent, "2.2.7", now));
        assert!(check_due_from(Some("2.2.7"), recent, "2.2.7", now));
        assert!(check_due_from(Some("2.2.6"), recent, "2.2.7", now));
        assert!(!check_due_from(Some("2.2.8"), recent, "2.2.7", now));
        assert!(check_due_from(
            Some("2.2.8"),
            now - THROTTLE.as_secs(),
            "2.2.7",
            now
        ));
    }
}

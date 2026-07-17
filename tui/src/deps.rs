//! Optional runtime dependency auto-install for the bone binary.
//!
//! None of these are required for the base app; they only power certain tools.

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

pub fn ensure_deps() {
    // Only check/install once. Marker lives under bone_dir so BONE_DIR/XDG
    // isolation works. Skip entirely when no config root is resolvable so
    // `bone --help` still works in a stripped environment.
    let Some(marker) = bone_core::config::try_bone_dir().map(|d| d.join(".deps-warned")) else {
        return;
    };
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

    if let Some(parent) = marker.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&marker, "");
}

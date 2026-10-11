//! What differs between Unix and Windows, for both sides: the shell commands
//! run in, how a command's whole process tree is stopped, the home directory.

use std::path::PathBuf;
use std::process::{Command, ExitStatus};

/// The shell commands run in, for tool descriptions and prompts.
pub fn shell_name() -> &'static str {
    if cfg!(windows) { "PowerShell" } else { "bash" }
}

/// A command that runs `script` in the shell: `bash -c` on Unix, PowerShell
/// (`pwsh` when installed) on Windows.
pub fn shell(script: &str) -> Command {
    #[cfg(unix)]
    {
        let mut c = Command::new("bash");
        c.arg("-c").arg(script);
        c
    }
    #[cfg(windows)]
    {
        let mut c = Command::new(powershell());
        c.args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            script,
        ]);
        c
    }
}

#[cfg(windows)]
fn powershell() -> &'static str {
    static FOUND: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
    FOUND.get_or_init(|| {
        let pwsh = Command::new("pwsh")
            .args(["-NoLogo", "-NoProfile", "-Command", "exit 0"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        if pwsh.is_ok_and(|s| s.success()) {
            "pwsh"
        } else {
            "powershell"
        }
    })
}

/// `program` as `Command::new` should get it. On Windows a bare name finds
/// only `NAME.exe` there, so `npx` and other `.cmd`/`.bat` scripts on the
/// PATH are looked up here.
pub fn program(program: &str) -> std::ffi::OsString {
    #[cfg(windows)]
    if std::path::Path::new(program).extension().is_none()
        && !program.contains(['/', '\\'])
        && let Some(path) = std::env::var_os("PATH")
    {
        for dir in std::env::split_paths(&path) {
            if dir.join(format!("{program}.exe")).is_file() {
                break;
            }
            for ext in ["cmd", "bat"] {
                let script = dir.join(format!("{program}.{ext}"));
                if script.is_file() {
                    return script.into_os_string();
                }
            }
        }
    }
    program.into()
}

/// Start the command as the leader of a new process group (Unix), so
/// [`terminate_tree`] and [`kill_tree`] reach everything it starts.
pub fn new_group(cmd: &mut Command) {
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(cmd, 0);
    #[cfg(not(unix))]
    let _ = cmd;
}

/// Ask a process group started with [`new_group`] to stop (SIGTERM). Windows
/// has no polite equivalent for console programs, so there it is
/// [`kill_tree`].
pub fn terminate_tree(pid: u32) {
    #[cfg(unix)]
    // SAFETY: kill(2) on our own child's process group.
    unsafe {
        libc::kill(-(pid as i32), libc::SIGTERM);
    }
    #[cfg(windows)]
    kill_tree(pid);
}

/// Stop a process and everything it started, now.
pub fn kill_tree(pid: u32) {
    #[cfg(unix)]
    // SAFETY: kill(2) on our own child's process group.
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
    #[cfg(windows)]
    {
        use std::process::Stdio;
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Whether anything of the process group may still be running. Only a
/// definite "no such group" counts as gone; Windows cannot tell, so says no
/// once the leader has exited.
pub fn tree_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: signal 0 only checks the group exists.
        let result = unsafe { libc::kill(-(pid as i32), 0) };
        result == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

/// The signal that ended the process, on Unix.
pub fn exit_signal(status: &ExitStatus) -> Option<i32> {
    #[cfg(unix)]
    {
        std::os::unix::process::ExitStatusExt::signal(status)
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        None
    }
}

/// The user's home directory (`$HOME`, `%USERPROFILE%` on Windows).
pub fn home_dir() -> Option<PathBuf> {
    std::env::home_dir().filter(|h| !h.as_os_str().is_empty())
}

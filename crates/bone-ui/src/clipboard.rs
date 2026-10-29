//! Reading an image (or text) for the prompt: from a file, or from the
//! clipboard through a disposable helper process (`bone --clipboard-read`),
//! so a native hang can be killed. Both frontends use it.
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::process::Command;

/// Where to read from.
pub enum Input {
    Clipboard,
    File(PathBuf),
}
/// What was read: text from a clipboard that holds no image, or an image's
/// bytes (the core validates and normalizes them on upload).
pub enum Content {
    Text(String),
    Image { bytes: Vec<u8>, name: String },
}

/// Read `input`; `executable` is the helper (the running binary if `None`).
pub async fn read(input: Input, executable: Option<PathBuf>) -> Result<Content, String> {
    match input {
        Input::Clipboard => {
            let executable = executable
                .map(Ok)
                .unwrap_or_else(running_exe)
                .map_err(|e| format!("cannot locate clipboard helper: {e}"))?;
            let mut cmd = Command::new(executable);
            cmd.arg("--clipboard-read");
            for (key, value) in display_env() {
                match value {
                    Some(value) => cmd.env(key, value),
                    None => cmd.env_remove(key),
                };
            }
            // Avoid opening a console window for the helper on Windows.
            #[cfg(windows)]
            cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
            read_helper(cmd, Duration::from_secs(10)).await
        }
        Input::File(path) => tokio::time::timeout(Duration::from_secs(10), file(path))
            .await
            .map_err(|_| "image file read timed out")?,
    }
}

/// The running binary. On Linux `/proc/self/exe` still execs it after the
/// file was replaced (a rebuild or reinstall), when `current_exe` names a
/// path that no longer exists.
fn running_exe() -> std::io::Result<PathBuf> {
    #[cfg(target_os = "linux")]
    if std::path::Path::new("/proc/self/exe").exists() {
        return Ok(PathBuf::from("/proc/self/exe"));
    }
    std::env::current_exe()
}

/// The variables that decide which clipboard is reachable, `None` when
/// unset. Inside tmux a pane's shell keeps the environment it started with,
/// which goes stale once the session is attached from elsewhere (the local
/// desktop after SSH, or the reverse), so tmux's session environment wins.
pub fn display_env() -> Vec<(&'static str, Option<OsString>)> {
    const KEYS: [&str; 3] = ["WAYLAND_DISPLAY", "DISPLAY", "SSH_CONNECTION"];
    let tmux = std::env::var_os("TMUX")
        .filter(|v| !v.is_empty())
        .and_then(|_| {
            std::process::Command::new("tmux")
                .arg("show-environment")
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output()
                .ok()
        })
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    KEYS.into_iter()
        .map(|key| {
            let session = tmux.lines().find_map(|line| {
                if line.strip_prefix('-') == Some(key) {
                    Some(None)
                } else {
                    let value = line.strip_prefix(key)?.strip_prefix('=')?;
                    Some(Some(OsString::from(value)))
                }
            });
            let value = session.unwrap_or_else(|| std::env::var_os(key));
            (key, value.filter(|v| !v.is_empty()))
        })
        .collect()
}

async fn file(path: PathBuf) -> Result<Content, String> {
    let f = tokio::fs::File::open(&path)
        .await
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let mut bytes = Vec::new();
    f.take(bone_media::MAX_IMAGE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|e| e.to_string())?;
    if bytes.len() > bone_media::MAX_IMAGE_BYTES {
        return Err("image exceeds the 20 MiB limit".into());
    }
    // The core validates and normalizes file bytes once, on upload.
    Ok(Content::Image {
        bytes,
        name: path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
    })
}

async fn read_helper(mut cmd: Command, deadline: Duration) -> Result<Content, String> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("cannot start clipboard helper: {e}"))?;
    let mut stdout = child
        .stdout
        .take()
        .unwrap()
        .take(bone_media::MAX_IMAGE_BYTES as u64 + 2);
    let mut bytes = Vec::new();
    let result = tokio::time::timeout(deadline, async {
        stdout
            .read_to_end(&mut bytes)
            .await
            .map_err(|e| format!("clipboard helper failed: {e}"))?;
        if bytes.len() > bone_media::MAX_IMAGE_BYTES + 1 {
            return Err("clipboard contents exceed the 20 MiB limit".into());
        }
        if !child.wait().await.map_err(|e| e.to_string())?.success() {
            return Err("clipboard helper failed".into());
        }
        if bytes.is_empty() {
            return Err("invalid clipboard helper response".into());
        }
        match bytes.remove(0) {
            b'I' => Ok(Content::Image {
                bytes,
                name: "Screenshot".into(),
            }),
            b'T' => String::from_utf8(bytes)
                .map(Content::Text)
                .map_err(|e| e.to_string()),
            b'E' => Err(String::from_utf8_lossy(&bytes).into_owned()),
            _ => Err("invalid clipboard helper response".into()),
        }
    })
    .await
    .unwrap_or_else(|_| Err("clipboard read timed out; try again or use /attach PATH".into()));
    // Stop and reap unfinished helpers on timeout or excessive output.
    if result.is_err() && child.id().is_some() {
        child
            .kill()
            .await
            .map_err(|e| format!("cannot stop clipboard helper: {e}"))?;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_kills_and_reaps_helper_then_next_read_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let mut stalled = Command::new("sh");
        stalled
            .args(["-c", "echo $$ > \"$1\"; exec sleep 30", "helper"])
            .arg(&pid_file);
        let result = read_helper(stalled, Duration::from_millis(250)).await;
        assert!(matches!(result, Err(e) if e.contains("timed out")));
        let pid: i32 = std::fs::read_to_string(pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // wait()/kill() must reap the process, not merely abandon a worker.
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        let mut healthy = Command::new("sh");
        healthy.args(["-c", "printf Tready"]);
        assert!(matches!(read_helper(healthy, Duration::from_secs(2)).await,
            Ok(Content::Text(text)) if text == "ready"));
    }
}

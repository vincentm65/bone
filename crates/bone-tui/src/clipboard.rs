//! Clipboard calls run in a disposable process so native hangs can be killed.
use std::io::Write;
use std::path::PathBuf;
use std::process::{ExitCode, Stdio};
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::process::Command;

pub(crate) enum Input {
    Clipboard,
    File(PathBuf),
}
pub(crate) enum Content {
    Text(String),
    Image { bytes: Vec<u8>, name: String },
}

pub(crate) async fn read(input: Input, executable: Option<PathBuf>) -> Result<Content, String> {
    match input {
        Input::Clipboard => {
            let executable = executable
                .map(Ok)
                .unwrap_or_else(std::env::current_exe)
                .map_err(|e| format!("cannot locate clipboard helper: {e}"))?;
            let mut cmd = Command::new(executable);
            cmd.arg("--clipboard-read");
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

/// Internal CLI entry point, invoked before loading any user configuration.
#[doc(hidden)]
pub fn run_clipboard_helper() -> ExitCode {
    let (tag, bytes) = match clipboard() {
        Ok(Content::Image { bytes, .. }) => (b'I', bytes),
        Ok(Content::Text(text)) => (b'T', text.into_bytes()),
        Err(error) => (b'E', error.into_bytes()),
    };
    let mut stdout = std::io::stdout().lock();
    match stdout
        .write_all(&[tag])
        .and_then(|_| stdout.write_all(&bytes))
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
fn clipboard() -> Result<Content, String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|e| format!("clipboard unavailable: {e}. Use /attach PATH when running remotely or without a desktop."))?;
    for attempt in 0..3 {
        match clipboard.get_image() {
            Ok(image) => {
                let png = bone_media::from_rgba(image.width, image.height, &image.bytes)?;
                return Ok(Content::Image {
                    bytes: png.bytes,
                    name: "Screenshot".into(),
                });
            }
            Err(arboard::Error::ContentNotAvailable) => {}
            Err(arboard::Error::ClipboardOccupied) if attempt < 2 => {
                std::thread::sleep(std::time::Duration::from_millis(40));
                continue;
            }
            Err(e) => return Err(format!("cannot read clipboard image: {e}")),
        }
        match clipboard.get_text() {
            Ok(text) if !text.is_empty() => return Ok(Content::Text(text)),
            Ok(_) | Err(arboard::Error::ContentNotAvailable) => {
                return Err("clipboard contains no supported image or text".into());
            }
            Err(arboard::Error::ClipboardOccupied) if attempt < 2 => {
                std::thread::sleep(std::time::Duration::from_millis(40))
            }
            Err(e) => return Err(format!("cannot read clipboard text: {e}")),
        }
    }
    Err("clipboard is busy; try pasting again".into())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn clipboard() -> Result<Content, String> {
    Err("image clipboard is unavailable on this platform; use /attach PATH".into())
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
    /// Run under Xvfb or a desktop: exercises actual native clipboard decoding.
    #[test]
    #[ignore = "requires a graphical clipboard; run with DISPLAY or WAYLAND_DISPLAY"]
    fn native_clipboard_round_trip() {
        let mut owner = arboard::Clipboard::new().unwrap();
        let pixels = vec![255, 0, 0, 255, 0, 255, 0, 255];
        owner
            .set_image(arboard::ImageData {
                width: 2,
                height: 1,
                bytes: std::borrow::Cow::Borrowed(&pixels),
            })
            .unwrap();
        match clipboard().unwrap() {
            Content::Image { bytes, .. } => {
                let decoded = bone_media::normalize(&bytes).unwrap();
                assert_eq!((decoded.width, decoded.height), (2, 1));
            }
            _ => panic!("image became text"),
        }
        owner.set_text("plain text\nsecond line").unwrap();
        match clipboard().unwrap() {
            Content::Text(text) => assert_eq!(text, "plain text\nsecond line"),
            _ => panic!("text became an image"),
        }
        owner.clear().unwrap();
        assert!(clipboard().is_err());
    }
}

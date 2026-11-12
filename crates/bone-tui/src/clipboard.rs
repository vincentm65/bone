//! The clipboard helper process (see `bone_ui::clipboard`, which runs it).
use std::io::Write;
use std::process::ExitCode;

pub(crate) use bone_ui::clipboard::{Content, Input, read};

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

/// Linux: through `wl-paste` (Wayland) or `xclip` (X11), the programs copying
/// already uses, so no clipboard library is built in.
#[cfg(target_os = "linux")]
fn clipboard() -> Result<Content, String> {
    tools::read(|key| std::env::var_os(key).is_some_and(|v| !v.is_empty()))
}

#[cfg(target_os = "linux")]
mod tools {
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    use super::Content;

    /// A clipboard program: how to list what the clipboard offers and how to
    /// read one type (the type is appended), and what it says on stderr when
    /// the clipboard is empty.
    pub(super) struct Tool {
        pub program: &'static str,
        package: &'static str,
        list: &'static [&'static str],
        get: &'static [&'static str],
        empty: &'static [&'static str],
    }

    pub(super) const WL_PASTE: Tool = Tool {
        program: "wl-paste",
        package: "wl-clipboard",
        list: &["--list-types"],
        get: &["--no-newline", "--type"],
        empty: &["Nothing is copied", "No selection"],
    };
    pub(super) const XCLIP: Tool = Tool {
        program: "xclip",
        package: "xclip",
        list: &["-selection", "clipboard", "-o", "-t", "TARGETS"],
        get: &["-selection", "clipboard", "-o", "-t"],
        empty: &["not available"],
    };

    enum Problem {
        /// The program is not installed.
        Missing,
        /// It ran and the clipboard holds nothing to paste.
        Empty,
        Failed(String),
    }

    /// The image type to ask for, of those the core can decode.
    pub(super) fn image_type(types: &str) -> Option<&str> {
        ["image/png", "image/jpeg", "image/webp"]
            .into_iter()
            .find_map(|want| types.lines().find(|t| t.trim().eq_ignore_ascii_case(want)))
            .map(str::trim)
    }

    /// The text type to ask for, UTF-8 ones first.
    pub(super) fn text_type(types: &str) -> Option<&str> {
        [
            "text/plain;charset=utf-8",
            "UTF8_STRING",
            "text/plain",
            "STRING",
            "TEXT",
        ]
        .into_iter()
        .find_map(|want| types.lines().find(|t| t.trim().eq_ignore_ascii_case(want)))
        .map(str::trim)
    }

    impl Tool {
        /// Run it and collect its output, at most one image's worth. The
        /// child dies with this helper, which is killed when a read hangs.
        fn run(&self, args: &[&str]) -> Result<Vec<u8>, Problem> {
            let mut cmd = Command::new(self.program);
            cmd.args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            unsafe {
                cmd.pre_exec(|| {
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                    Ok(())
                });
            }
            let mut child = cmd.spawn().map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => Problem::Missing,
                _ => Problem::Failed(format!("cannot run {}: {e}", self.program)),
            })?;
            let mut stderr = child.stderr.take().unwrap();
            let errors = std::thread::spawn(move || {
                let mut text = String::new();
                let _ = (&mut stderr).take(4096).read_to_string(&mut text);
                text
            });
            let limit = bone_media::MAX_IMAGE_BYTES as u64 + 1;
            let mut out = Vec::new();
            let read = child.stdout.take().unwrap().take(limit).read_to_end(&mut out);
            if out.len() as u64 >= limit {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Problem::Failed(
                    "clipboard contents exceed the 20 MiB limit".into(),
                ));
            }
            let status = child.wait();
            let errors = errors.join().unwrap_or_default();
            match (read, status) {
                (Ok(_), Ok(status)) if status.success() => Ok(out),
                _ if self.empty.iter().any(|m| errors.contains(m)) => Err(Problem::Empty),
                _ => Err(Problem::Failed(format!(
                    "cannot read clipboard: {} failed: {}",
                    self.program,
                    errors.trim().lines().next().unwrap_or("no output")
                ))),
            }
        }

        fn get(&self, kind: &str) -> Result<Vec<u8>, Problem> {
            let mut args = self.get.to_vec();
            args.push(kind);
            self.run(&args)
        }

        /// An image when the clipboard offers one, else its text.
        fn read(&self) -> Result<Content, Problem> {
            let types = String::from_utf8_lossy(&self.run(self.list)?).into_owned();
            if let Some(kind) = image_type(&types) {
                return Ok(Content::Image {
                    bytes: self.get(kind)?,
                    name: "Screenshot".into(),
                });
            }
            let Some(kind) = text_type(&types) else {
                return Err(Problem::Empty);
            };
            let text = String::from_utf8_lossy(&self.get(kind)?).into_owned();
            if text.is_empty() {
                return Err(Problem::Empty);
            }
            Ok(Content::Text(text))
        }
    }

    /// Read with the program of each display there is, Wayland first.
    /// `set` tells whether an environment variable has a value.
    pub(super) fn read(set: impl Fn(&str) -> bool) -> Result<Content, String> {
        const REMOTE: &str = "Use /attach PATH when running remotely or without a desktop.";
        let tools: Vec<&Tool> = [("WAYLAND_DISPLAY", &WL_PASTE), ("DISPLAY", &XCLIP)]
            .into_iter()
            .filter(|(key, _)| set(key))
            .map(|(_, tool)| tool)
            .collect();
        if tools.is_empty() {
            return Err(format!("clipboard unavailable: no display. {REMOTE}"));
        }
        let mut failed = None;
        for tool in &tools {
            match tool.read() {
                Ok(content) => return Ok(content),
                Err(Problem::Empty) => {
                    return Err("clipboard contains no supported image or text".into());
                }
                Err(Problem::Missing) => {}
                Err(Problem::Failed(e)) => failed = failed.or(Some(e)),
            }
        }
        Err(match failed {
            Some(e) => e,
            None => format!(
                "clipboard unavailable: install {} to paste. {REMOTE}",
                tools
                    .iter()
                    .map(|t| t.package)
                    .collect::<Vec<_>>()
                    .join(" or ")
            ),
        })
    }
}

#[cfg(any(target_os = "macos", windows))]
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

#[cfg(all(test, any(target_os = "linux", target_os = "macos", windows)))]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn picks_an_image_the_core_decodes_then_utf8_text() {
        use tools::{image_type, text_type};
        let browser = "text/html\nimage/webp\nIMAGE/PNG\nimage/bmp\n";
        assert_eq!(image_type(browser), Some("IMAGE/PNG"));
        assert_eq!(image_type("image/bmp\nimage/jpeg\n"), Some("image/jpeg"));
        assert_eq!(image_type("image/bmp\ntext/plain\n"), None);
        let x11 = "TARGETS\nSTRING\nUTF8_STRING\ntext/plain\n";
        assert_eq!(text_type(x11), Some("UTF8_STRING"));
        assert_eq!(
            text_type("text/plain\ntext/plain;charset=utf-8\n"),
            Some("text/plain;charset=utf-8")
        );
        assert_eq!(text_type("text/uri-list\nx-special/files\n"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn no_display_or_no_program_says_what_to_do() {
        let none = tools::read(|_| false).err().unwrap();
        assert!(none.contains("no display") && none.contains("/attach PATH"));
    }

    /// Put `data` on the clipboard as `kind` (text when `None`), the way a
    /// desktop program would: `wl-copy`, or `xclip` under X11.
    #[cfg(target_os = "linux")]
    fn own(kind: Option<&str>, data: &[u8]) {
        use std::process::{Command, Stdio};
        let mut cmd = if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            let mut cmd = Command::new("wl-copy");
            cmd.args(kind.iter().flat_map(|k| ["--type", k]));
            cmd
        } else {
            let mut cmd = Command::new("xclip");
            cmd.args(["-selection", "clipboard", "-i"]);
            cmd.args(kind.iter().flat_map(|k| ["-t", k]));
            cmd
        };
        // The program stays behind to serve the data; it must not hold the
        // test's output open.
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(data).unwrap();
        assert!(child.wait().unwrap().success());
    }

    /// Run on a desktop: exercises the real clipboard programs. It replaces
    /// what the clipboard holds.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires a graphical clipboard; run with DISPLAY or WAYLAND_DISPLAY"]
    fn native_clipboard_round_trip() {
        let png = bone_media::from_rgba(2, 1, &[255, 0, 0, 255, 0, 255, 0, 255]).unwrap();
        own(Some("image/png"), &png.bytes);
        match clipboard().unwrap() {
            Content::Image { bytes, .. } => {
                let decoded = bone_media::normalize(&bytes).unwrap();
                assert_eq!((decoded.width, decoded.height), (2, 1));
            }
            _ => panic!("image became text"),
        }
        own(None, b"plain text\nsecond line");
        match clipboard().unwrap() {
            Content::Text(text) => assert_eq!(text, "plain text\nsecond line"),
            _ => panic!("text became an image"),
        }
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            let cleared = std::process::Command::new("wl-copy").arg("--clear").status();
            assert!(cleared.unwrap().success());
            assert!(clipboard().is_err());
        }
    }

    /// Run under a desktop: exercises actual native clipboard decoding.
    #[cfg(any(target_os = "macos", windows))]
    #[test]
    #[ignore = "requires a graphical clipboard; run on a desktop"]
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

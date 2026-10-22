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

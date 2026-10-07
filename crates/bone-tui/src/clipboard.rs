//! Clipboard and file workers keep image decoding off the UI thread.
use std::io::Read;
use std::path::PathBuf;
use std::sync::{OnceLock, mpsc};

pub(crate) enum Input {
    Clipboard,
    File(PathBuf),
}
pub(crate) enum Content {
    Text(String),
    Image { png: Vec<u8>, name: String },
}
type Reply = Box<dyn FnOnce(Result<Content, String>) + Send>;
type Job = (Input, Reply);

pub(crate) fn read(input: Input, reply: impl FnOnce(Result<Content, String>) + Send + 'static) {
    static CLIPBOARD_WORKER: OnceLock<mpsc::SyncSender<Job>> = OnceLock::new();
    static FILE_WORKER: OnceLock<mpsc::SyncSender<Job>> = OnceLock::new();
    let slot = match &input {
        Input::Clipboard => &CLIPBOARD_WORKER,
        Input::File(_) => &FILE_WORKER,
    };
    let worker = slot.get_or_init(|| {
        let (tx, rx) = mpsc::sync_channel::<Job>(16);
        std::thread::Builder::new()
            .name("bone-clipboard".into())
            .spawn(move || {
                for (input, reply) in rx {
                    let result = match input {
                        Input::Clipboard => clipboard(),
                        Input::File(path) => file(path),
                    };
                    reply(result);
                }
            })
            .expect("clipboard worker starts");
        tx
    });
    if let Err(e) = worker.try_send((input, Box::new(reply))) {
        let (mpsc::TrySendError::Full((_, reply)) | mpsc::TrySendError::Disconnected((_, reply))) =
            e;
        reply(Err("clipboard worker is busy; try again in a moment".into()));
    }
}

fn file(path: PathBuf) -> Result<Content, String> {
    let f =
        std::fs::File::open(&path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let mut bytes = Vec::new();
    f.take(bone_media::MAX_IMAGE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    let png = bone_media::normalize(&bytes)?;
    Ok(Content::Image {
        png: png.bytes,
        name: path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
    })
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
fn clipboard() -> Result<Content, String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|e| format!("clipboard unavailable: {e}. Use /attach PATH when running remotely or without a desktop."))?;
    for attempt in 0..3 {
        match clipboard.get_image() {
            Ok(image) => {
                let png = bone_media::from_rgba(image.width, image.height, &image.bytes)?;
                return Ok(Content::Image {
                    png: png.bytes,
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
            Content::Image { png, .. } => {
                let decoded = bone_media::normalize(&png).unwrap();
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

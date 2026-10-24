//! The pictures of the node view (`image = { id | path }`): their pixels are
//! read once (from the core for an attachment, from disk for a path),
//! decoded off the UI thread and kept as a texture. Where and how they are
//! shown is Lua's: an image node in a chat item, or the overlay that
//! `bone.gui.preview` opens.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use base64::Engine;
use bone_proto::methods::{AttachmentRead, AttachmentReadParams};
use eframe::egui;

use crate::Gui;
use crate::node::ImageSource;

/// The largest side a texture is decoded at.
const MAX_SIDE: u32 = 2048;
/// Textures past this many bytes are dropped, least recently shown first.
const BUDGET: usize = 256 << 20;
/// A failed load is tried again after this long (a file that was missing).
const FAIL_RETRY: Duration = Duration::from_secs(5);

#[derive(Default)]
pub(crate) struct Previews {
    textures: RefCell<HashMap<String, Preview>>,
}

enum Preview {
    Loading,
    Ready {
        tex: egui::TextureHandle,
        bytes: usize,
        last_used: Instant,
    },
    Failed {
        error: String,
        at: Instant,
    },
}

/// What an image node can draw now.
pub(crate) enum Pixels {
    Loading,
    Ready(egui::TextureHandle),
    Failed(String),
}

impl Gui {
    /// Whether the image with this key (an attachment's id) was read and can
    /// be drawn.
    pub fn image_shown(&self, id: &str) -> bool {
        matches!(
            self.previews.textures.borrow().get(id),
            Some(Preview::Ready { .. })
        )
    }

    /// The pixels of `source`; the first ask starts reading them.
    pub(crate) fn pixels(&self, ctx: &egui::Context, source: &ImageSource) -> Pixels {
        let key = source.key();
        let now = Instant::now();
        let known = {
            let mut map = self.previews.textures.borrow_mut();
            match map.get_mut(&key) {
                Some(Preview::Ready { tex, last_used, .. }) => {
                    *last_used = now;
                    Some(Pixels::Ready(tex.clone()))
                }
                Some(Preview::Failed { error, at }) if now.duration_since(*at) < FAIL_RETRY => {
                    Some(Pixels::Failed(error.clone()))
                }
                Some(Preview::Failed { .. }) => None,
                Some(Preview::Loading) => Some(Pixels::Loading),
                None => None,
            }
        };
        if let Some(known) = known {
            return known;
        }
        self.previews
            .textures
            .borrow_mut()
            .insert(key.clone(), Preview::Loading);
        self.load_pixels(ctx, key, source.clone());
        Pixels::Loading
    }

    /// Drop the least recently shown textures once they pass [`BUDGET`].
    pub(crate) fn sweep_previews(&self) {
        let mut map = self.previews.textures.borrow_mut();
        let mut total = 0usize;
        let mut ready = Vec::new();
        for (key, preview) in map.iter() {
            if let Preview::Ready {
                bytes, last_used, ..
            } = preview
            {
                total += bytes;
                ready.push((key.clone(), *last_used, *bytes));
            }
        }
        if total <= BUDGET {
            return;
        }
        ready.sort_by_key(|(_, at, _)| *at);
        for (key, _, bytes) in ready {
            if total <= BUDGET {
                break;
            }
            map.remove(&key);
            total = total.saturating_sub(bytes);
        }
    }

    fn load_pixels(&self, ctx: &egui::Context, key: String, source: ImageSource) {
        let ctx = ctx.clone();
        // Decoded off the UI thread: an image can be large.
        let decode = move |png: Vec<u8>, key: String| {
            let (w, h, pixels) = bone_media::to_rgba(&png, MAX_SIDE)?;
            let pixels =
                egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &pixels);
            Ok::<_, String>(ctx.load_texture(&key, pixels, egui::TextureOptions::LINEAR))
        };
        let finish = |gui: &mut Gui, key: String, r: Result<egui::TextureHandle, String>| {
            let preview = match r {
                Ok(tex) => {
                    let [w, h] = tex.size();
                    Preview::Ready {
                        bytes: w.saturating_mul(h).saturating_mul(4),
                        last_used: Instant::now(),
                        tex,
                    }
                }
                Err(e) => Preview::Failed {
                    error: format!("cannot show the image: {e}"),
                    at: Instant::now(),
                },
            };
            gui.previews.textures.borrow_mut().insert(key, preview);
        };
        match source {
            ImageSource::Attachment(id) => {
                let params = AttachmentReadParams { id };
                self.request::<AttachmentRead>(params, move |gui, r| {
                    let k = key.clone();
                    let work = move || {
                        let png = base64::engine::general_purpose::STANDARD
                            .decode(r?.data)
                            .map_err(|e| e.to_string())?;
                        decode(png, k)
                    };
                    let decoded = async move {
                        (tokio::task::spawn_blocking(work).await)
                            .unwrap_or_else(|e| Err(e.to_string()))
                    };
                    bone_ui::lua::Host::spawn(gui, decoded, move |gui, r| finish(gui, key, r));
                });
            }
            ImageSource::Path(path) => {
                let k = key.clone();
                let work = move || {
                    let bytes = std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
                    let png = bone_media::normalize(&bytes)?;
                    decode(png.bytes, k)
                };
                let decoded = async move {
                    (tokio::task::spawn_blocking(work).await).unwrap_or_else(|e| Err(e.to_string()))
                };
                // Not through the core: this machine's file.
                let (tx, repaint) = (self.tx.clone(), self.repaint.clone());
                self.rt.spawn(async move {
                    let r = decoded.await;
                    let _ = tx.send(crate::Msg::Apply(Box::new(move |gui| finish(gui, key, r))));
                    crate::wake(&repaint);
                });
            }
        }
    }
}

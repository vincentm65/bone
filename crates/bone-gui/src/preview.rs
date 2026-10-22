//! Images in the chat and under the prompt: a row that names an image (as
//! the standard views' `[Image: name · w×h]` do) shows it on hover, and a
//! click opens it in a window. The pixels are read from the core once and
//! kept as a texture.

use std::cell::RefCell;
use std::collections::HashMap;

use base64::Engine;
use bone_proto::methods::{AttachmentRead, AttachmentReadParams};
use bone_proto::types::ImageAttachment;
use eframe::egui;

use crate::Gui;

/// The largest side a texture is decoded at.
const MAX_SIDE: u32 = 2048;
/// The largest side of a preview on hover, in points.
const HOVER_SIDE: f32 = 320.0;

#[derive(Default)]
pub(crate) struct Previews {
    textures: RefCell<HashMap<String, Preview>>,
    /// The image open in a window.
    open: RefCell<Option<ImageAttachment>>,
}

enum Preview {
    Loading,
    Ready(egui::TextureHandle),
    Failed(String),
}

impl Gui {
    /// If row `text` (drawn at `resp`) names one of `images`, show that
    /// image while it is hovered and open it when it is clicked.
    pub(crate) fn image_row(&self, resp: &egui::Response, text: &str, images: &[ImageAttachment]) {
        let Some(image) = images
            .iter()
            .find(|i| !i.name.is_empty() && text.contains(&i.name))
        else {
            return;
        };
        if resp.clicked() {
            *self.previews.open.borrow_mut() = Some(image.clone());
        }
        if resp.hovered() {
            resp.ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
            resp.clone()
                .on_hover_ui(|ui| self.image_ui(ui, image, HOVER_SIDE));
        }
    }

    /// Whether image `id` was read and can be drawn.
    pub fn image_shown(&self, id: &str) -> bool {
        matches!(
            self.previews.textures.borrow().get(id),
            Some(Preview::Ready(_))
        )
    }

    /// Chat row `row` of `chat`, drawn at `resp`: the images of its item.
    pub(crate) fn chat_image_row(
        &self,
        chat: &crate::ChatBuffer,
        row: usize,
        text: &str,
        resp: &egui::Response,
    ) {
        let Some((index, _)) = chat.item_at_row(row) else {
            return;
        };
        let Some(&item) = chat.items().get(index - 1) else {
            return;
        };
        let images = chat.item_data(item, index, false)["images"].take();
        let images: Vec<ImageAttachment> = serde_json::from_value(images).unwrap_or_default();
        if !images.is_empty() {
            self.image_row(resp, text, &images);
        }
    }

    /// The image open in a window, if any.
    pub(crate) fn preview_ui(&self, ctx: &egui::Context) {
        let Some(image) = self.previews.open.borrow().clone() else {
            return;
        };
        let mut open = true;
        let side = ctx.content_rect().size().min_elem() * 0.8;
        egui::Window::new(&image.name)
            .id(egui::Id::new("image preview"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| self.image_ui(ui, &image, side));
        if !open || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            *self.previews.open.borrow_mut() = None;
        }
    }

    /// `image` scaled to fit `side` points, once it is read.
    fn image_ui(&self, ui: &mut egui::Ui, image: &ImageAttachment, side: f32) {
        let state = (self.previews.textures.borrow().get(&image.id)).map(|p| match p {
            Preview::Ready(tex) => Some(Ok(tex.clone())),
            Preview::Failed(e) => Some(Err(e.clone())),
            Preview::Loading => None,
        });
        let Some(state) = state else {
            self.load_preview(ui.ctx(), &image.id);
            ui.weak("loading…");
            return;
        };
        match state {
            Some(Ok(tex)) => {
                let size = tex.size_vec2();
                let scale = (side / size.max_elem()).min(1.0);
                ui.image((tex.id(), size * scale));
                ui.weak(format!("{} · {}×{}", image.name, image.width, image.height));
            }
            Some(Err(e)) => {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            None => {
                ui.weak("loading…");
            }
        }
    }

    fn load_preview(&self, ctx: &egui::Context, id: &str) {
        (self.previews.textures.borrow_mut()).insert(id.to_owned(), Preview::Loading);
        let (id, ctx) = (id.to_owned(), ctx.clone());
        let params = AttachmentReadParams { id: id.clone() };
        self.request::<AttachmentRead>(params, move |gui, r| {
            // Decoded off the UI thread: an image can be large.
            let key = id.clone();
            let decode = move || {
                let png = base64::engine::general_purpose::STANDARD
                    .decode(r?.data)
                    .map_err(|e| e.to_string())?;
                let (w, h, pixels) = bone_media::to_rgba(&png, MAX_SIDE)?;
                let pixels =
                    egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &pixels);
                Ok::<_, String>(ctx.load_texture(&id, pixels, egui::TextureOptions::LINEAR))
            };
            let decoded = async move {
                (tokio::task::spawn_blocking(decode).await).unwrap_or_else(|e| Err(e.to_string()))
            };
            bone_ui::lua::Host::spawn(gui, decoded, move |gui, r| {
                let preview = match r {
                    Ok(tex) => Preview::Ready(tex),
                    Err(e) => Preview::Failed(format!("cannot show the image: {e}")),
                };
                gui.previews.textures.borrow_mut().insert(key, preview);
            });
        });
    }
}

//! Images in the prompt: read from a file (`/attach`, or dropped on the
//! window) or the clipboard (`paste_clipboard`, through the helper process
//! of `bone_ui::clipboard`), uploaded to the core, and sent with the next
//! message. egui pastes the clipboard's text itself on ctrl+v, so an image
//! is pasted with another key bound to `paste_clipboard` (alt+v), or
//! `/paste`.

use std::path::PathBuf;

use base64::Engine;
use bone_proto::methods::{AttachmentUpload, AttachmentUploadParams};
use bone_proto::types::ImageAttachment;
use bone_ui::clipboard::{self, Content, Input};
use bone_ui::lua::{args, err, ret};
use eframe::egui;
use mlua::{Lua, MultiValue};

use crate::Gui;

/// The image ops; `None` for an op that is not one of them.
pub(crate) fn dispatch(
    gui: &mut Gui,
    lua: &Lua,
    op: &str,
    a: MultiValue,
) -> Option<mlua::Result<MultiValue>> {
    Some(match op {
        "clipboard_paste" => {
            gui.paste_clipboard();
            ret(lua, ())
        }
        "image_attach" => args::<String>(lua, a).and_then(|path| {
            gui.attach_file(path.into());
            ret(lua, ())
        }),
        "image_remove" => args::<usize>(lua, a).and_then(|index| {
            if index == 0 || index > gui.images.len() {
                return Err(err("image index is out of range"));
            }
            gui.images.remove(index - 1);
            gui.prompt_changed();
            ret(lua, ())
        }),
        _ => return None,
    })
}

impl Gui {
    /// Attach image file `path` (relative to the chat's directory).
    pub fn attach_file(&mut self, path: PathBuf) {
        let path = if path.is_absolute() {
            path
        } else {
            let cwd = (self.chats[self.current].session.as_ref())
                .map_or(self.opts.cwd.as_str(), |s| s.cwd.as_str());
            PathBuf::from(cwd).join(path)
        };
        self.read_image(Input::File(path));
    }

    /// The clipboard's image, or its text into the prompt.
    pub fn paste_clipboard(&mut self) {
        self.read_image(Input::Clipboard);
    }

    /// Files dropped on the window this frame are attached.
    pub(crate) fn dropped_files(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        for f in dropped {
            self.attach_file(f.path().to_owned());
        }
    }

    fn read_image(&mut self, input: Input) {
        if matches!(input, Input::File(_))
            && self.images.len() + self.image_reads >= bone_media::MAX_IMAGES
        {
            return self.error("a message can contain at most 8 images".into());
        }
        let epoch = self.draft_epoch;
        self.image_reads += 1;
        self.info("reading clipboard/image…".into());
        bone_ui::lua::Host::spawn(self, clipboard::read(input, None), move |gui, r| match r {
            Ok(Content::Image { bytes, name }) => {
                let params = AttachmentUploadParams {
                    data: base64::engine::general_purpose::STANDARD.encode(bytes),
                    name,
                };
                gui.request::<AttachmentUpload>(params, move |gui, r| {
                    gui.image_reads -= 1;
                    match r {
                        Ok(image) => gui.deliver_image(epoch, image),
                        Err(e) => gui.error(format!("cannot attach image: {e}")),
                    }
                });
            }
            Ok(Content::Text(text)) => {
                gui.image_reads -= 1;
                if epoch == gui.draft_epoch {
                    gui.draft.push_str(&text);
                    gui.prompt_changed();
                }
            }
            Err(e) => {
                gui.image_reads -= 1;
                gui.error(e);
            }
        });
    }

    /// An uploaded image joins the draft it was read for, if that is still
    /// the draft.
    fn deliver_image(&mut self, epoch: u64, image: ImageAttachment) {
        if epoch != self.draft_epoch {
            return;
        }
        let total: u64 = self.images.iter().map(|i| i.bytes).sum();
        if self.images.len() >= bone_media::MAX_IMAGES
            || total + image.bytes > bone_media::MAX_MESSAGE_IMAGE_BYTES
        {
            return self.error(
                "image would exceed the message attachment limit; remove an image first".into(),
            );
        }
        self.images.push(image.clone());
        self.fire_event("image/attached", serde_json::json!({ "image": image }));
        self.prompt_changed();
        self.info("image attached · enter sends · /detach removes".into());
    }
}

//! Durable, content-addressed PNGs, independent of a client's filesystem.
//! Blobs are retained while any session (including forks) can refer to them.
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use base64::{Engine, engine::general_purpose::STANDARD};
use bone_proto::types::{ChatMessage, ImageAttachment};
use sha2::{Digest, Sha256};

pub(crate) struct AttachmentStore {
    dir: PathBuf,
}

impl AttachmentStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.join("attachments"),
        }
    }

    fn path(&self, id: &str) -> Result<PathBuf, String> {
        if id.len() != 64
            || !id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("invalid attachment id".into());
        }
        Ok(self.dir.join(format!("{id}.png")))
    }

    pub fn upload(&self, data: &str, name: &str) -> Result<ImageAttachment, String> {
        if data.len() > bone_media::MAX_IMAGE_BYTES.div_ceil(3) * 4 {
            return Err("image exceeds the 20 MiB limit".into());
        }
        let bytes = STANDARD
            .decode(data)
            .map_err(|_| "image data must be valid base64")?;
        let png = bone_media::normalize(&bytes)?;
        let id = format!("{:x}", Sha256::digest(&png.bytes));
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| format!("cannot create attachment storage: {e}"))?;
        let path = self.path(&id)?;
        if !path.exists() {
            let mut tmp = tempfile::NamedTempFile::new_in(&self.dir).map_err(|e| e.to_string())?;
            tmp.write_all(&png.bytes).map_err(|e| e.to_string())?;
            tmp.as_file().sync_all().map_err(|e| e.to_string())?;
            match tmp.persist_noclobber(&path) {
                Ok(_) => {}
                Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(format!("cannot save attachment: {}", e.error)),
            }
        }
        let name: String = name.chars().filter(|c| !c.is_control()).take(120).collect();
        Ok(ImageAttachment {
            id,
            name: if name.trim().is_empty() {
                "Screenshot".into()
            } else {
                name
            },
            mime_type: "image/png".into(),
            width: png.width,
            height: png.height,
            bytes: png.bytes.len() as u64,
            data: None,
        })
    }

    fn read_bytes(&self, id: &str) -> Result<Vec<u8>, String> {
        let path = self.path(id)?;
        let file =
            std::fs::File::open(path).map_err(|e| format!("cannot read attachment {id}: {e}"))?;
        let mut bytes = Vec::new();
        file.take(bone_media::MAX_IMAGE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > bone_media::MAX_IMAGE_BYTES {
            return Err("stored image exceeds the 20 MiB limit".into());
        }
        if format!("{:x}", Sha256::digest(&bytes)) != id {
            return Err("stored image is corrupt".into());
        }
        Ok(bytes)
    }

    pub fn read(&self, id: &str) -> Result<String, String> {
        Ok(STANDARD.encode(self.read_bytes(id)?))
    }

    pub fn validate(&self, images: &mut [ImageAttachment]) -> Result<(), String> {
        self.resolve(images, false)
    }

    fn resolve(&self, images: &mut [ImageAttachment], hydrate: bool) -> Result<(), String> {
        if images.len() > bone_media::MAX_IMAGES {
            return Err("a message can contain at most 8 images".into());
        }
        let mut total = 0;
        for img in images {
            if !hydrate && img.data.is_some() {
                return Err(
                    "upload image bytes with attachment/upload before referencing them".into(),
                );
            }
            let bytes = self.read_bytes(&img.id)?;
            // Hash-verified canonical bytes determine metadata, never caller input.
            let (width, height) = bone_media::png_dimensions(&bytes)?;
            img.mime_type = "image/png".into();
            img.width = width;
            img.height = height;
            img.bytes = bytes.len() as u64;
            img.name = img
                .name
                .chars()
                .filter(|c| !c.is_control())
                .take(120)
                .collect();
            total += img.bytes;
            if hydrate {
                img.data = Some(STANDARD.encode(bytes));
            }
        }
        if total > bone_media::MAX_MESSAGE_IMAGE_BYTES {
            return Err("message images exceed the 40 MiB limit".into());
        }
        Ok(())
    }

    pub fn hydrate(&self, messages: &[ChatMessage]) -> Result<Vec<ChatMessage>, String> {
        let mut messages = messages.to_vec();
        for msg in &mut messages {
            if let ChatMessage::User { images, .. } = msg {
                self.resolve(images, true)?;
            }
        }
        Ok(messages)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, AttachmentStore, String) {
        let dir = tempfile::tempdir().unwrap();
        let store = AttachmentStore::new(dir.path());
        let png = bone_media::from_rgba(2, 1, &[255, 0, 0, 255, 0, 255, 0, 0]).unwrap();
        (dir, store, STANDARD.encode(png.bytes))
    }

    #[test]
    fn deduplicates_and_canonicalizes_untrusted_references() {
        let (_dir, store, data) = fixture();
        let mut image = store.upload(&data, "screen\n\u{1b}.png").unwrap();
        assert_eq!(image.name, "screen.png");
        assert_eq!(image.id, store.upload(&data, "again").unwrap().id);
        assert_eq!(std::fs::read_dir(&store.dir).unwrap().count(), 1);
        image.width = u32::MAX;
        image.height = 0;
        image.bytes = 0;
        image.mime_type = "text/plain".into();
        store.validate(std::slice::from_mut(&mut image)).unwrap();
        assert_eq!((image.width, image.height), (2, 1));
        assert_eq!(image.mime_type, "image/png");
        assert!(image.bytes > 0);
        let hydrated = store
            .hydrate(&[ChatMessage::User {
                content: String::new(),
                images: vec![image],
            }])
            .unwrap();
        let ChatMessage::User { images, .. } = &hydrated[0] else {
            panic!()
        };
        assert_eq!(images[0].data.as_deref(), Some(data.as_str()));
    }

    #[test]
    fn rejects_invalid_missing_corrupt_and_excessive_attachments() {
        let (_dir, store, data) = fixture();
        assert!(store.upload("not base64!", "screen").is_err());
        assert!(
            store
                .upload(&STANDARD.encode(b"not an image"), "screen")
                .is_err()
        );
        assert!(store.read("../core.lua").is_err());
        assert!(store.read(&"0".repeat(64)).is_err());
        let image = store.upload(&data, "screen").unwrap();
        assert!(
            store
                .validate(&mut vec![image.clone(); bone_media::MAX_IMAGES + 1])
                .is_err()
        );
        let mut inline = image.clone();
        inline.data = Some(data);
        assert!(store.validate(&mut [inline]).is_err());
        std::fs::write(store.path(&image.id).unwrap(), b"corrupt").unwrap();
        assert!(store.read(&image.id).unwrap_err().contains("corrupt"));
        assert!(
            store
                .validate(&mut [image])
                .unwrap_err()
                .contains("corrupt")
        );
    }
}

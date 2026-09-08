//! Bounded, local-only image decoding and egui previews.
//!
//! The caller supplies the already-authoritative image payload. This module does
//! not open paths or perform network requests; `name` is only a display/cache key.

use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::io::Cursor;

use base64::Engine;
use eframe::egui;

const MAX_ENCODED_BYTES: usize = 20 * 1024 * 1024;
const MAX_DECODED_BYTES: usize = 15 * 1024 * 1024;
const MAX_PIXELS: u64 = 40_000_000;
const MAX_DIMENSION: u32 = 8_192;
const DEFAULT_MAX_ENTRIES: usize = 32;
const DEFAULT_MAX_CACHE_BYTES: usize = 64 * 1024 * 1024;

struct CachedImage {
    texture: egui::TextureHandle,
    width: u32,
    height: u32,
    bytes: usize,
}

/// Small LRU cache for image textures. The cache owns decoded thumbnails, not
/// source bytes, and is safe to reuse across transcript redraws.
pub struct ImageCache {
    entries: HashMap<String, CachedImage>,
    order: VecDeque<String>,
    bytes: usize,
    max_entries: usize,
    max_bytes: usize,
    failures: HashMap<String, String>,
    failure_order: VecDeque<String>,
    preview: Option<String>,
}

impl Default for ImageCache {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_ENTRIES, DEFAULT_MAX_CACHE_BYTES)
    }
}

impl ImageCache {
    pub fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            bytes: 0,
            max_entries: max_entries.max(1),
            max_bytes: max_bytes.max(1),
            failures: HashMap::new(),
            failure_order: VecDeque::new(),
            preview: None,
        }
    }

    /// Remove all decoded textures and close any open larger preview.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.bytes = 0;
        self.failures.clear();
        self.failure_order.clear();
        self.preview = None;
    }

    /// Alias useful when the parent replaces a conversation/tab.
    pub fn reset(&mut self) {
        self.clear();
    }

    /// Render a thumbnail. Clicking it opens a larger, still locally decoded
    /// preview window. Invalid or oversized payloads are reported inline.
    ///
    /// `key` must be a stable identifier for `(name, media_type, data_b64)`
    /// (see [`cache_key`]). It is supplied by the caller so the payload is not
    /// re-hashed on every frame. Returns true when a thumbnail was shown.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        key: &str,
        name: &str,
        media_type: &str,
        data_b64: &str,
    ) -> bool {
        let key = key.to_owned();
        if !self.entries.contains_key(&key) {
            if let Some(error) = self.failures.get(&key) {
                ui.colored_label(egui::Color32::from_rgb(190, 70, 70), error);
                return false;
            }
            match decode(ui.ctx(), name, media_type, data_b64) {
                Ok(image) => {
                    if !self.insert(key.clone(), image) {
                        let error = "image exceeds the preview cache limit".to_owned();
                        self.remember_failure(key, error.clone());
                        ui.colored_label(egui::Color32::from_rgb(190, 70, 70), error);
                        return false;
                    }
                }
                Err(error) => {
                    self.remember_failure(key, error.clone());
                    ui.colored_label(egui::Color32::from_rgb(190, 70, 70), error);
                    return false;
                }
            }
        }
        self.touch(&key);

        let (texture, width, height) = {
            let image = self.entries.get(&key).expect("inserted above");
            (image.texture.clone(), image.width, image.height)
        };
        let thumbnail = egui::vec2(160.0, 120.0);
        let scale = (thumbnail.x / width as f32)
            .min(thumbnail.y / height as f32)
            .min(1.0);
        let size = egui::vec2(width as f32 * scale, height as f32 * scale);
        let response = ui.add(
            egui::Image::new(egui::load::SizedTexture::new(texture.id(), size))
                .sense(egui::Sense::click()),
        );
        if response.clicked() {
            self.preview = Some(key.clone());
        }
        response.on_hover_text(format!("{name} ({width}×{height}) — click to enlarge"));
        true
    }

    /// Render the currently selected image preview once for a tab/window.
    /// `id` must be unique per parent pane so previews cannot collide.
    pub fn show_preview(&mut self, ctx: &egui::Context, id: impl Hash) {
        let Some(preview_key) = self.preview.clone() else {
            return;
        };
        let Some(image) = self.entries.get(&preview_key) else {
            self.preview = None;
            return;
        };
        let mut open = true;
        let screen = ctx.content_rect().size();
        let max_width = (screen.x * 0.9).clamp(320.0, 1200.0);
        let max_height = (screen.y * 0.8).clamp(240.0, 900.0);
        let mut preview_id = std::collections::hash_map::DefaultHasher::new();
        id.hash(&mut preview_id);
        egui::Window::new("Image preview")
            .id(egui::Id::new(("bone-image-preview", preview_id.finish())))
            .open(&mut open)
            .resizable(true)
            .max_width(max_width)
            .max_height(max_height)
            .show(ctx, |ui| {
                let scale = (max_width / image.width as f32)
                    .min(max_height / image.height as f32)
                    .min(1.0);
                ui.add(egui::Image::new(egui::load::SizedTexture::new(
                    image.texture.id(),
                    egui::vec2(image.width as f32 * scale, image.height as f32 * scale),
                )));
            });
        if !open {
            self.preview = None;
        }
    }

    fn remember_failure(&mut self, key: String, error: String) {
        if self.failures.contains_key(&key) {
            return;
        }
        while self.failures.len() >= self.max_entries {
            let Some(old) = self.failure_order.pop_front() else {
                break;
            };
            self.failures.remove(&old);
        }
        self.failure_order.push_back(key.clone());
        self.failures.insert(key, error);
    }

    fn insert(&mut self, key: String, image: CachedImage) -> bool {
        let bytes = image.bytes;
        if bytes > self.max_bytes {
            return false;
        }
        while self.entries.len() >= self.max_entries || self.bytes + bytes > self.max_bytes {
            let Some(old) = self.order.pop_front() else {
                break;
            };
            if let Some(old_image) = self.entries.remove(&old) {
                self.bytes = self.bytes.saturating_sub(old_image.bytes);
            }
        }
        self.bytes += bytes;
        self.order.push_back(key.clone());
        self.entries.insert(key, image);
        true
    }

    fn touch(&mut self, key: &str) {
        if let Some(position) = self.order.iter().position(|item| item == key) {
            self.order.remove(position);
            self.order.push_back(key.to_owned());
        }
    }
}

/// Stable cache key for an image payload. Hashes the full base64 once so the
/// per-frame render path can reuse it instead of re-hashing multi-megabyte data.
pub fn cache_key(name: &str, media_type: &str, data_b64: &str) -> String {
    // Hash the payload rather than retaining a potentially multi-megabyte base64
    // string in every bounded cache key.
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    name.hash(&mut hasher);
    media_type.hash(&mut hasher);
    data_b64.hash(&mut hasher);
    format!("{name}\0{media_type}\0{:016x}", hasher.finish())
}

fn decode(
    ctx: &egui::Context,
    name: &str,
    media_type: &str,
    data_b64: &str,
) -> Result<CachedImage, String> {
    if !matches!(
        media_type,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    ) {
        return Err(format!("{name}: unsupported image media type"));
    }
    if data_b64.len() > MAX_ENCODED_BYTES {
        return Err(format!("{name}: encoded image exceeds the size limit"));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data_b64)
        .map_err(|_| format!("{name}: invalid base64 image data"))?;
    if bytes.len() > MAX_DECODED_BYTES {
        return Err(format!("{name}: image exceeds the decoded size limit"));
    }
    let reader = image::ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .map_err(|_| format!("{name}: unsupported image format"))?;
    let (width, height) = reader
        .into_dimensions()
        .map_err(|_| format!("{name}: invalid image header"))?;
    if width == 0
        || height == 0
        || width > MAX_DIMENSION
        || height > MAX_DIMENSION
        || u64::from(width) * u64::from(height) > MAX_PIXELS
    {
        return Err(format!("{name}: image dimensions are too large"));
    }
    let image = image::load_from_memory(&bytes)
        .map_err(|_| format!("{name}: image data could not be decoded"))?
        .to_rgba8();
    let actual_bytes = image.as_raw().len();
    if actual_bytes > MAX_DECODED_BYTES {
        return Err(format!("{name}: decoded pixels exceed the size limit"));
    }
    let color =
        egui::ColorImage::from_rgba_unmultiplied([width as usize, height as usize], image.as_raw());
    let texture = ctx.load_texture(name.to_owned(), color, egui::TextureOptions::LINEAR);
    Ok(CachedImage {
        texture,
        width,
        height,
        bytes: actual_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_and_oversized_data_is_rejected_before_decode() {
        let ctx = egui::Context::default();
        assert!(decode(&ctx, "bad", "image/png", "not-base64").is_err());
        let oversized = "A".repeat(MAX_ENCODED_BYTES + 1);
        assert!(decode(&ctx, "large", "image/png", &oversized).is_err());
    }

    #[test]
    fn failure_cache_is_bounded() {
        let mut cache = ImageCache::new(2, 1024);
        cache.remember_failure("a".into(), "bad".into());
        cache.remember_failure("b".into(), "bad".into());
        cache.remember_failure("c".into(), "bad".into());
        assert_eq!(cache.failures.len(), 2);
        assert!(!cache.failures.contains_key("a"));
    }
}

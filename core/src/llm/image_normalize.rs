//! Model-bound image normalization.
//!
//! Tools may attach arbitrarily large captures (screenshots, photos read via
//! `read_file`), but providers are happiest — and token budgets safest — with
//! bounded image dimensions. This stage runs at the model boundary and
//! downscales everything oversized to at most [`MAX_MODEL_IMAGE_WIDTH`] ×
//! [`MAX_MODEL_IMAGE_HEIGHT`].
//!
//! Work is kept off the hot path: an image whose declared dimensions are
//! already within bounds is returned without touching its payload, a payload is
//! decoded exactly once, and the dimensions this stage measures are recorded so
//! later rounds of the same turn skip the work again (and the token estimate
//! sees real pixels).
//!
//! The bound is only as good as the declared dimensions: the fast path trusts
//! them, so a caller that understates the real size would still ship an
//! oversized payload (nothing built in does — tools sniff the header when they
//! attach an image). Sizes that this stage measures itself always replace the
//! declared ones. The pass is deliberately non-fatal: anything that cannot be
//! decoded or re-encoded (unsupported format, corrupt data, missing decoder) is
//! passed through untouched, because a resize hiccup must never block a turn.

use std::io::Cursor;

use base64::Engine;
use image::{DynamicImage, ImageFormat};
use sha2::{Digest, Sha256};

use super::{ChatMessage, ImageData};

/// Maximum width of an image sent to the model, in pixels.
pub const MAX_MODEL_IMAGE_WIDTH: u32 = 1920;
/// Maximum height of an image sent to the model, in pixels.
pub const MAX_MODEL_IMAGE_HEIGHT: u32 = 1080;

/// Downscale every oversized image in `messages`, in place.
pub fn normalize_images_in_messages(messages: &mut [ChatMessage]) {
    for image in messages.iter_mut().flat_map(|m| m.images.iter_mut()) {
        normalize_image(image);
    }
}

/// Whether declared dimensions already fit the model bounds.
fn in_bounds(width: Option<u32>, height: Option<u32>) -> bool {
    matches!(
        (width, height),
        (Some(w), Some(h))
            if w > 0 && h > 0 && w <= MAX_MODEL_IMAGE_WIDTH && h <= MAX_MODEL_IMAGE_HEIGHT
    )
}

/// Largest size that preserves the aspect ratio within the model bounds.
fn fit_within_bounds(width: u32, height: u32) -> (u32, u32) {
    let scale = (f64::from(MAX_MODEL_IMAGE_WIDTH) / f64::from(width))
        .min(f64::from(MAX_MODEL_IMAGE_HEIGHT) / f64::from(height))
        .min(1.0);
    (
        ((f64::from(width) * scale).round() as u32).max(1),
        ((f64::from(height) * scale).round() as u32).max(1),
    )
}

/// The encoding to use for the downscaled copy, with the media type it must be
/// declared as so that the provider decodes what it actually receives.
fn output_format(source: &[u8], has_alpha: bool) -> (ImageFormat, &'static str) {
    // PNG and GIF start lossless (screenshots, diagrams, transparency), so keep
    // them exact; JPEG and WebP are usually captured photos, where PNG costs an
    // order of magnitude more bytes for no visible gain.
    let lossless_source = matches!(
        image::guess_format(source),
        Ok(ImageFormat::Png | ImageFormat::Gif)
    );
    if lossless_source || has_alpha {
        (ImageFormat::Png, "image/png")
    } else {
        (ImageFormat::Jpeg, "image/jpeg")
    }
}

/// Downscale a single image to the model bounds, in place.
fn normalize_image(image: &mut ImageData) {
    // Fast path: trust declared dimensions when they already fit, so an
    // in-bounds image costs no base64 or image decode at all.
    if in_bounds(image.width, image.height) {
        return;
    }

    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&image.data) else {
        return;
    };
    let Ok(loaded) = image::load_from_memory(&bytes) else {
        return;
    };

    // The decoded payload is authoritative, not the (possibly stale or
    // optimistic) declared fields.
    let (width, height) = (loaded.width(), loaded.height());
    if in_bounds(Some(width), Some(height)) {
        // Nothing to resize, but record the real size: the next round then
        // takes the fast path and the token estimate stops guessing.
        image.width = Some(width);
        image.height = Some(height);
        return;
    }

    let (new_w, new_h) = fit_within_bounds(width, height);
    // Box-average downscale: integer block sampling, allocation limited to the
    // target buffer, and anti-aliased by construction (unlike nearest).
    let resized = image::imageops::thumbnail(&loaded, new_w, new_h);

    let (format, media_type) = output_format(&bytes, loaded.color().has_alpha());
    let mut out = Vec::new();
    // Cursor<&mut Vec<u8>> satisfies the Write + Seek bound of `write_to` and
    // avoids the extra buffer `into_inner` would need.
    if DynamicImage::ImageRgba8(resized)
        .write_to(&mut Cursor::new(&mut out), format)
        .is_err()
    {
        return;
    }

    image.data = base64::engine::general_purpose::STANDARD.encode(&out);
    // The payload was just re-encoded, so the declared type follows the encoder
    // rather than the source: providers reject a mismatched data URL.
    image.media_type = media_type.to_string();
    image.width = Some(new_w);
    image.height = Some(new_h);
    if image.sha256.is_some() {
        image.sha256 = Some(format!("{:x}", Sha256::digest(&out)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::parse_image_dimensions;
    use image::RgbaImage;

    fn make_png(width: u32, height: u32) -> Vec<u8> {
        let img = RgbaImage::from_fn(width, height, |x, y| {
            image::Rgba([((x * 7) % 256) as u8, ((y * 13) % 256) as u8, 128, 255])
        });
        let mut out = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .expect("encode png");
        out
    }

    /// Two-tone RGB fixture for encoders that cannot take the RGBA gradient
    /// above (GIF palettes cap at 256 colours).
    fn make_file(width: u32, height: u32, format: image::ImageFormat) -> Vec<u8> {
        let img = image::RgbImage::from_fn(width, height, |x, y| {
            if (x / 16 + y / 16) % 2 == 0 {
                image::Rgb([200, 30, 30])
            } else {
                image::Rgb([20, 20, 220])
            }
        });
        let mut out = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut out), format)
            .expect("encode fixture");
        out
    }

    fn image_data(bytes: Vec<u8>) -> ImageData {
        ImageData {
            media_type: "image/png".to_string(),
            data: base64::engine::general_purpose::STANDARD.encode(&bytes),
            ..Default::default()
        }
    }

    /// The pass runs per message, so tests go through the public entry point.
    fn normalize_one(img: ImageData) -> ImageData {
        let mut message = ChatMessage::new(crate::llm::ChatRole::User, "look".to_string());
        message.images = vec![img];
        normalize_images_in_messages(std::slice::from_mut(&mut message));
        message.images.pop().expect("single image")
    }

    #[test]
    fn small_image_is_untouched() {
        let bytes = make_png(640, 480);
        let mut img = image_data(bytes);
        img.width = Some(640);
        img.height = Some(480);
        let original = img.data.clone();
        img = normalize_one(img);
        assert_eq!(img.data, original);
    }

    #[test]
    fn oversized_png_is_resized_to_bounds() {
        let bytes = make_png(3840, 2160);
        let mut img = image_data(bytes);
        img.width = Some(3840);
        img.height = Some(2160);
        img.sha256 = Some("old".to_string());
        img = normalize_one(img);
        assert_eq!(img.width, Some(1920));
        assert_eq!(img.height, Some(1080));
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&img.data)
            .expect("valid base64");
        let (w, h) = parse_image_dimensions(&decoded).expect("png dimensions");
        assert_eq!((w, h), (1920, 1080));
        // sha256 was present, so it must now match the resized bytes.
        let expected = format!("{:x}", Sha256::digest(&decoded));
        assert_eq!(img.sha256.as_deref(), Some(expected.as_str()));
        assert_ne!(img.sha256.as_deref(), Some("old"));
    }

    #[test]
    fn width_only_oversize_keeps_aspect_ratio() {
        let bytes = make_png(3000, 500);
        let mut img = image_data(bytes);
        img.width = Some(3000);
        img.height = Some(500);
        img = normalize_one(img);
        assert_eq!(img.width, Some(1920));
        assert_eq!(img.height, Some(320));
    }

    #[test]
    fn unknown_dimensions_are_measured() {
        // No width/height set: the stage must decode to measure.
        let bytes = make_png(2500, 1400);
        let img = image_data(bytes);
        let img = normalize_one(img);
        // 2500x1400 scales by min(1920/2500, 1080/1400) = 0.768 → 1920x1075.
        assert_eq!(img.width, Some(1920));
        assert_eq!(img.height, Some(1075));
    }

    #[test]
    fn corrupt_data_passes_through() {
        let img = ImageData {
            media_type: "image/png".to_string(),
            data: "not-base64!!".to_string(),
            width: Some(99999),
            height: Some(99999),
            ..Default::default()
        };
        let before = img.clone();
        let img = normalize_one(img);
        assert_eq!(img, before);
    }

    #[test]
    fn unsupported_format_passes_through() {
        // Valid base64, unknown image bytes, oversized declared dimensions.
        let bytes = vec![0u8; 4096];
        let mut img = image_data(bytes);
        img.width = Some(99999);
        img.height = Some(99999);
        let before = img.clone();
        let img = normalize_one(img);
        assert_eq!(img, before);
    }

    fn make_jpeg(width: u32, height: u32) -> Vec<u8> {
        let img = image::RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([((x * 7) % 256) as u8, ((y * 13) % 256) as u8, 64])
        });
        let mut out = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut out),
            image::ImageFormat::Jpeg,
        )
        .expect("encode jpeg");
        out
    }

    fn decoded(img: &ImageData) -> Vec<u8> {
        base64::engine::general_purpose::STANDARD
            .decode(&img.data)
            .expect("valid base64")
    }

    /// JPEG input must come back as a *valid JPEG* at the bounded size: the
    /// resize re-encodes through `DynamicImage`, which is free to change the
    /// colour type, so the payload format is worth asserting explicitly.
    #[test]
    fn oversized_jpeg_is_resized_and_stays_jpeg() {
        let mut img = ImageData {
            media_type: "image/jpeg".to_string(),
            data: base64::engine::general_purpose::STANDARD.encode(make_jpeg(2400, 1400)),
            width: Some(2400),
            height: Some(1400),
            ..Default::default()
        };
        img = normalize_one(img);

        let bytes = decoded(&img);
        assert_eq!(&bytes[..2], &[0xFF, 0xD8], "still JPEG (SOI marker)");
        assert!(
            image::load_from_memory(&bytes).is_ok(),
            "resized JPEG must decode"
        );
        let (w, h) = parse_image_dimensions(&bytes).expect("jpeg dimensions");
        assert_eq!((w, h), (1851, 1080));
        assert_eq!((img.width, img.height), (Some(1851), Some(1080)));
        assert_eq!(img.media_type, "image/jpeg");
    }

    /// Oversized JPEG with no declared dimensions: the stage sniffs the header
    /// and still bounds the payload.
    #[test]
    fn oversized_jpeg_without_dimensions_is_resized() {
        let mut img = ImageData {
            media_type: "image/jpeg".to_string(),
            data: base64::engine::general_purpose::STANDARD.encode(make_jpeg(2400, 1400)),
            ..Default::default()
        };
        img = normalize_one(img);
        assert_eq!(img.width, Some(1851));
        assert_eq!(img.height, Some(1080));
    }

    /// The driver calls the pass once per request round, so re-running it must
    /// be a no-op rather than resampling (and degrading) the image again.
    #[test]
    fn normalization_is_idempotent() {
        let mut img = ImageData {
            media_type: "image/jpeg".to_string(),
            data: base64::engine::general_purpose::STANDARD.encode(make_jpeg(2400, 1400)),
            width: Some(2400),
            height: Some(1400),
            sha256: Some("old".to_string()),
        };
        img = normalize_one(img);
        let once = img.clone();
        let twice = normalize_one(img);
        assert_eq!(once, twice);
    }

    /// Every image in every message must be visited, including tool-role relays.
    #[test]
    fn all_images_in_all_messages_are_normalized() {
        let oversized = |media: &str| ImageData {
            media_type: media.to_string(),
            data: base64::engine::general_purpose::STANDARD.encode(if media == "image/jpeg" {
                make_jpeg(3000, 2000)
            } else {
                make_png(3000, 2000)
            }),
            width: Some(3000),
            height: Some(2000),
            ..Default::default()
        };
        let mut msgs = vec![
            ChatMessage::new(crate::llm::ChatRole::User, "look".to_string()),
            ChatMessage::tool(Default::default()),
        ];
        msgs[0].images = vec![oversized("image/png"), oversized("image/jpeg")];
        msgs[1].images = vec![oversized("image/png")];

        normalize_images_in_messages(&mut msgs);

        for msg in &msgs {
            for img in &msg.images {
                let (w, h) = parse_image_dimensions(&decoded(img)).expect("dimensions");
                // 3000x2000 scales by min(1920/3000, 1080/2000) = 0.54.
                assert_eq!((w, h), (1620, 1080), "{} bounded", img.media_type);
                assert_eq!((img.width, img.height), (Some(1620), Some(1080)));
            }
        }
    }

    #[test]
    fn messages_without_images_are_untouched() {
        let msg = ChatMessage::new(crate::llm::ChatRole::User, "hello".to_string());
        let mut msgs = vec![msg];
        normalize_images_in_messages(&mut msgs);
        assert_eq!(msgs[0].content, "hello");
        assert!(msgs[0].images.is_empty());
    }

    /// An in-bounds image with no declared dimensions is not re-encoded, but its
    /// measured size is recorded: the next round skips decoding it and the token
    /// estimate sees real pixels instead of a guess.
    #[test]
    fn dimension_less_bounded_image_records_dimensions() {
        let mut img = image_data(make_png(800, 600));
        let original = img.data.clone();
        img = normalize_one(img);
        assert_eq!(img.data, original, "payload must stay byte-identical");
        assert_eq!((img.width, img.height), (Some(800), Some(600)));
    }

    /// Stale or optimistic declared dimensions must not drive the resize: the
    /// decoded payload decides, so a small image is never upscaled.
    #[test]
    fn declared_oversize_does_not_upscale_a_small_payload() {
        let mut img = image_data(make_png(800, 600));
        img.width = Some(4000);
        img.height = Some(3000);
        let original = img.data.clone();
        img = normalize_one(img);
        assert_eq!(img.data, original, "payload must not be resampled");
        assert_eq!((img.width, img.height), (Some(800), Some(600)));
    }

    /// A GIF source stays lossless, but the payload is re-encoded to PNG: the
    /// media type has to follow the encoder, or the provider receives a PNG
    /// mislabelled as `image/gif` and fails to decode it.
    #[test]
    fn oversized_gif_is_resized_and_relabelled() {
        let mut img = ImageData {
            media_type: "image/gif".to_string(),
            data: base64::engine::general_purpose::STANDARD.encode(make_file(
                2400,
                1400,
                image::ImageFormat::Gif,
            )),
            width: Some(2400),
            height: Some(1400),
            ..Default::default()
        };
        img = normalize_one(img);

        let bytes = decoded(&img);
        assert_eq!(&bytes[..4], b"\x89PNG", "payload is PNG");
        assert_eq!(img.media_type, "image/png");
        assert_eq!(parse_image_dimensions(&bytes), Some((1851, 1080)));
        assert_eq!((img.width, img.height), (Some(1851), Some(1080)));
    }

    /// WebP is the common case for captured photos: re-encoding those through
    /// PNG costs an order of magnitude in payload bytes, so an opaque one is
    /// carried as JPEG instead.
    #[test]
    fn oversized_opaque_webp_is_carried_as_jpeg() {
        let mut img = ImageData {
            media_type: "image/webp".to_string(),
            data: base64::engine::general_purpose::STANDARD.encode(make_file(
                2400,
                1400,
                image::ImageFormat::WebP,
            )),
            width: Some(2400),
            height: Some(1400),
            ..Default::default()
        };
        img = normalize_one(img);

        let bytes = decoded(&img);
        assert_eq!(&bytes[..2], &[0xFF, 0xD8], "payload is JPEG");
        assert_eq!(img.media_type, "image/jpeg");
        assert!(image::load_from_memory(&bytes).is_ok(), "must decode");
        assert_eq!((img.width, img.height), (Some(1851), Some(1080)));
    }

    /// Whatever the caller declared, the media type must describe the bytes that
    /// are actually sent.
    #[test]
    fn mislabelled_payload_is_relabelled_to_its_real_type() {
        let mut img = image_data(make_png(3000, 2000));
        img.media_type = "image/webp".to_string();
        img.width = Some(3000);
        img.height = Some(2000);
        img = normalize_one(img);
        assert_eq!(img.media_type, "image/png");
        assert_eq!(&decoded(&img)[..4], b"\x89PNG");
    }
}

//! Image inputs are bounded, decoded by content, and normalized to lossless PNG.
use std::io::Cursor;

pub const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;
pub const MAX_IMAGE_PIXELS: u64 = 40_000_000;
pub const MAX_IMAGES: usize = 8;
pub const MAX_MESSAGE_IMAGE_BYTES: u64 = 40 * 1024 * 1024;

pub struct PngImage {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

fn dimensions(width: u32, height: u32) -> Result<(), String> {
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS {
        return Err("image dimensions are invalid or exceed 40 megapixels".into());
    }
    Ok(())
}

/// Inspect a canonical PNG without allocating or re-encoding its pixels.
pub fn png_dimensions(bytes: &[u8]) -> Result<(u32, u32), String> {
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err("image exceeds the 20 MiB limit".into());
    }
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err("stored attachment is not a PNG".into());
    }
    let (width, height) =
        image::ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::Png)
            .into_dimensions()
            .map_err(|e| format!("cannot read image dimensions: {e}"))?;
    dimensions(width, height)?;
    Ok((width, height))
}

pub fn normalize(bytes: &[u8]) -> Result<PngImage, String> {
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err("image exceeds the 20 MiB limit".into());
    }
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("cannot identify image: {e}"))?;
    if !matches!(
        reader.format(),
        Some(image::ImageFormat::Png | image::ImageFormat::Jpeg | image::ImageFormat::WebP)
    ) {
        return Err("unsupported image; use PNG, JPEG or WebP".into());
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_PIXELS as u32);
    limits.max_image_height = Some(MAX_IMAGE_PIXELS as u32);
    limits.max_alloc = Some(MAX_IMAGE_PIXELS * 8);
    reader.limits(limits);
    // Inspect dimensions before allocating decoded pixels.
    let (width, height) = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .into_dimensions()
        .map_err(|e| format!("cannot read image dimensions: {e}"))?;
    dimensions(width, height)?;
    let img = reader
        .decode()
        .map_err(|e| format!("cannot decode image: {e}"))?;
    encode(img)
}

pub fn from_rgba(width: usize, height: usize, bytes: &[u8]) -> Result<PngImage, String> {
    let width = u32::try_from(width).map_err(|_| "image width is too large")?;
    let height = u32::try_from(height).map_err(|_| "image height is too large")?;
    dimensions(width, height)?;
    let expected = u64::from(width) * u64::from(height) * 4;
    if expected != bytes.len() as u64 {
        return Err("clipboard returned incomplete image pixels".into());
    }
    let img = image::RgbaImage::from_raw(width, height, bytes.to_vec())
        .ok_or("invalid clipboard image")?;
    encode(image::DynamicImage::ImageRgba8(img))
}

fn encode(img: image::DynamicImage) -> Result<PngImage, String> {
    let width = img.width();
    let height = img.height();
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png)
        .map_err(|e| format!("cannot encode PNG: {e}"))?;
    let bytes = out.into_inner();
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err("normalized image exceeds the 20 MiB limit".into());
    }
    Ok(PngImage {
        bytes,
        width,
        height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normalizes_pixels_without_losing_transparency() {
        let pixels = [255, 0, 0, 255, 1, 2, 3, 0];
        let png = from_rgba(2, 1, &pixels).unwrap();
        let decoded = image::load_from_memory(&normalize(&png.bytes).unwrap().bytes)
            .unwrap()
            .into_rgba8();
        assert_eq!(decoded.as_raw(), &pixels);
    }
    #[test]
    fn accepts_jpeg_and_webp_by_content() {
        let image = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            3,
            2,
            image::Rgb([10, 20, 30]),
        ));
        for format in [image::ImageFormat::Jpeg, image::ImageFormat::WebP] {
            let mut input = Cursor::new(Vec::new());
            image.write_to(&mut input, format).unwrap();
            let png = normalize(input.get_ref()).unwrap();
            assert_eq!(png_dimensions(&png.bytes).unwrap(), (3, 2));
            assert_eq!(
                image::guess_format(&png.bytes).unwrap(),
                image::ImageFormat::Png
            );
        }
    }

    #[test]
    fn rejects_malformed_and_unbounded_images() {
        assert!(normalize(b"not an image").is_err());
        assert!(from_rgba(0, 1, &[]).is_err());
        assert!(from_rgba(100_000, 100_000, &[]).is_err());
        assert!(from_rgba(2, 1, &[0; 4]).is_err());
        assert!(normalize(&vec![0; MAX_IMAGE_BYTES + 1]).is_err());
    }
}

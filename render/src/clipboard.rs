//! Reading an image from the system clipboard (`wl-paste` under Wayland,
//! `arboard` elsewhere, with `xclip` as a fallback), as a PNG-or-better
//! attachment ready to send.

use std::process::Command;

use base64::Engine;
use bone_protocol::ImageData;

/// Read an image from the clipboard.
pub fn clipboard_image() -> Result<ImageData, String> {
    // Prefer the native Wayland command. `arboard` may select its X11 backend
    // under XWayland and wait for an unreachable X server before we ever reach
    // the working Wayland fallback.
    #[cfg(not(target_os = "android"))]
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        return wayland_clipboard_image().or_else(|wayland_err| {
            arboard_clipboard_image()
                .map_err(|arboard_err| format!("{wayland_err}; fallback failed: {arboard_err}"))
        });
    }

    // `arboard` has no Android backend, so there we rely solely on the external
    // clipboard command fallback.
    #[cfg(not(target_os = "android"))]
    match arboard_clipboard_image() {
        Ok(image) => Ok(image),
        Err(arboard_err) => match x11_clipboard_image() {
            Ok(image) => Ok(image),
            Err(external_err) => Err(format!("{arboard_err}; fallback failed: {external_err}")),
        },
    }

    #[cfg(target_os = "android")]
    x11_clipboard_image()
}

#[cfg(not(target_os = "android"))]
fn arboard_clipboard_image() -> Result<ImageData, String> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|err| format!("clipboard unavailable: {err}"))?;
    let image = clipboard
        .get_image()
        .map_err(|err| format!("clipboard has no image: {err}"))?;

    let mut png_bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut png_bytes, image.width as u32, image.height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .map_err(|err| format!("PNG header failed: {err}"))?
        .write_image_data(image.bytes.as_ref())
        .map_err(|err| format!("PNG encode failed: {err}"))?;

    Ok(png_image_data(png_bytes))
}

#[cfg(not(target_os = "android"))]
fn png_image_data(png_bytes: Vec<u8>) -> ImageData {
    ImageData {
        media_type: "image/png".to_string(),
        data: base64::engine::general_purpose::STANDARD.encode(png_bytes),
        ..Default::default()
    }
}

fn wayland_clipboard_image() -> Result<ImageData, String> {
    run_clipboard_command("wl-paste", &["--type", "image/png"])
        .or_else(|_| run_clipboard_command("wl-paste", &["--type", "image/jpeg"]))
        .or_else(|_| run_clipboard_command("wl-paste", &["--type", "image/webp"]))
}

fn x11_clipboard_image() -> Result<ImageData, String> {
    run_clipboard_command(
        "xclip",
        &["-selection", "clipboard", "-t", "image/png", "-o"],
    )
    .or_else(|_| {
        run_clipboard_command(
            "xclip",
            &["-selection", "clipboard", "-t", "image/jpeg", "-o"],
        )
    })
    .or_else(|_| {
        run_clipboard_command(
            "xclip",
            &["-selection", "clipboard", "-t", "image/webp", "-o"],
        )
    })
}

fn run_clipboard_command(command: &str, args: &[&str]) -> Result<ImageData, String> {
    let output = Command::new(command)
        .args(args)
        .output()
        .map_err(|err| format!("{command} failed to start: {err}"))?;
    if !output.status.success() || output.stdout.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            format!("{command} returned no image")
        } else {
            format!("{command}: {stderr}")
        });
    }

    let media_type = match args.last().copied() {
        Some("image/jpeg") => "image/jpeg",
        Some("image/webp") => "image/webp",
        _ => "image/png",
    };

    Ok(ImageData {
        media_type: media_type.to_string(),
        data: base64::engine::general_purpose::STANDARD.encode(output.stdout),
        ..Default::default()
    })
}

//! The fonts built into the window: Geist for text (regular, medium,
//! semibold, italic) and Geist Mono for code (SIL Open Font License; see
//! assets/fonts/OFL.txt). A face is named by a path `builtin:<file>`, so
//! the options that take a font file can name one too.

/// The proportional faces the window uses when no option names a file:
/// regular, bold (drawn semibold) and italic.
pub const SANS: [&str; 3] = [
    "builtin:Geist-Regular",
    "builtin:Geist-SemiBold",
    "builtin:Geist-Italic",
];

/// The medium face, loaded beside the regular one (same family, weight 500).
pub const SANS_MEDIUM: &str = "builtin:Geist-Medium";

/// The monospace face when `bone.o.font` is empty.
pub const MONO: &str = "builtin:GeistMono-Regular";

fn builtin(name: &str) -> Option<&'static [u8]> {
    Some(match name {
        "Geist-Regular" => include_bytes!("../assets/fonts/Geist-Regular.ttf"),
        "Geist-Medium" => include_bytes!("../assets/fonts/Geist-Medium.ttf"),
        "Geist-SemiBold" => include_bytes!("../assets/fonts/Geist-SemiBold.ttf"),
        "Geist-Italic" => include_bytes!("../assets/fonts/Geist-Italic.ttf"),
        "GeistMono-Regular" => include_bytes!("../assets/fonts/GeistMono-Regular.ttf"),
        _ => return None,
    })
}

/// A font file's bytes: a built-in face, or a file on disk.
pub fn read(path: &str) -> Option<Vec<u8>> {
    match path.strip_prefix("builtin:") {
        Some(name) => builtin(name).map(<[u8]>::to_vec),
        None => std::fs::read(path).ok(),
    }
}

//! `bone-android-preview`: the phone app in a phone-sized desktop window. Its
//! SSH key and pinned host keys live in `$XDG_DATA_HOME/bone-android` (or
//! `~/.local/share/bone-android`).

fn main() {
    let data_dir = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".local/share"))
        })
        .unwrap_or_default()
        .join("bone-android");
    if let Err(error) = bone_android::run_preview(data_dir) {
        eprintln!("bone-android-preview: {error}");
        std::process::exit(1);
    }
}

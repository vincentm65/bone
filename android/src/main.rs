//! `bone-android-preview [--ssh <user@host[:port]> [--bone <path>] | --connect <addr>]`:
//! the phone app in a phone-sized desktop window. The app's SSH key and pinned
//! host keys live in `$XDG_DATA_HOME/bone-android` (or `~/.local/share/...`).

use bone_android::Target;
use bone_android::ssh::Destination;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let target = match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] => None,
        ["--connect", address] => Some(Target::Local(address.to_string())),
        ["--ssh", destination, rest @ ..] => {
            let bone = match rest {
                [] => "bone".to_string(),
                ["--bone", path] => path.to_string(),
                _ => usage(),
            };
            match Destination::parse(destination) {
                Ok(destination) => Some(Target::Ssh { destination, bone }),
                Err(error) => {
                    eprintln!("bone-android-preview: {error}");
                    std::process::exit(2);
                }
            }
        }
        _ => usage(),
    };
    let data_dir = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".local/share"))
        })
        .unwrap_or_default()
        .join("bone-android");
    if let Err(error) = bone_android::run_preview(target, data_dir) {
        eprintln!("bone-android-preview: {error}");
        std::process::exit(1);
    }
}

fn usage() -> ! {
    eprintln!(
        "Usage: bone-android-preview [--ssh <user@host[:port]> [--bone <path>] | --connect <addr>]"
    );
    std::process::exit(2);
}

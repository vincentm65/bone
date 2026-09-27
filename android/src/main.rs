//! `bone-android-preview [--ssh <host> | --connect <addr>]`: the phone app in a
//! phone-sized desktop window.

use bone_android::Target;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let target = match args.as_slice() {
        [] => None,
        [flag, host] if flag == "--ssh" => Some(Target::Ssh(host.clone())),
        [flag, address] if flag == "--connect" => Some(Target::Local(address.clone())),
        _ => {
            eprintln!("Usage: bone-android-preview [--ssh <host> | --connect <addr>]");
            std::process::exit(2);
        }
    };
    if let Err(error) = bone_android::run_preview(target) {
        eprintln!("bone-android-preview: {error}");
        std::process::exit(1);
    }
}

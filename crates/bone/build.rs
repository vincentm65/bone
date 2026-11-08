//! On Windows, bone.exe carries the app's icon (assets/icon.ico, made by
//! scripts/icons.sh): Explorer, the taskbar and the window show it.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.rc");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("assets/icon.rc", embed_resource::NONE)
            .manifest_optional()
            .unwrap();
    }
}

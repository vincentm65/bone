use std::{env, path::Path};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("android") {
        return;
    }

    // LuaJIT calls __clear_cache on ARM. Rust links with -nodefaultlibs,
    // so explicitly include the compiler runtime that Clang would supply.
    // Use the same target-aware compiler discovery as vendored LuaJIT.
    let compiler = cc::Build::new().get_compiler();
    let output = compiler
        .to_command()
        .args(["--rtlib=compiler-rt", "--print-libgcc-file-name"])
        .output()
        .expect("failed to query the Android C compiler for its builtins archive");
    assert!(
        output.status.success(),
        "Android C compiler could not locate its builtins archive: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let archive =
        String::from_utf8(output.stdout).expect("Android compiler runtime path is not UTF-8");
    let archive = Path::new(archive.trim());
    assert!(
        archive.is_file(),
        "Android compiler builtins archive does not exist: {}",
        archive.display()
    );
    let library = archive
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("lib"))
        .and_then(|name| name.strip_suffix(".a"))
        .expect("expected an Android compiler runtime archive named lib*.a");

    // Native library metadata follows bone-lua into downstream links, including tests.
    println!(
        "cargo:rustc-link-search=native={}",
        archive.parent().unwrap().display()
    );
    println!("cargo:rustc-link-lib=static={library}");
    println!("cargo:rerun-if-changed={}", archive.display());
}

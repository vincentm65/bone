use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

fn command_output(command: &mut Command) -> Option<String> {
    let output = command.output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|value| !value.is_empty())
}

fn git_revision(manifest_dir: &Path) -> String {
    if let Ok(revision) = env::var("BONE_GIT_SHA")
        && !revision.trim().is_empty()
    {
        return revision.trim().chars().take(12).collect();
    }

    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(manifest_dir)
        .args(["rev-parse", "--short=12", "HEAD"]);
    let Some(mut revision) = command_output(&mut command) else {
        return "unknown".into();
    };

    let mut status = Command::new("git");
    status
        .arg("-C")
        .arg(manifest_dir)
        .args(["status", "--porcelain"]);
    if command_output(&mut status).is_some() {
        revision.push_str("-dirty");
    }
    revision
}

fn emit_build_identity(manifest_dir: &Path) {
    for name in ["BONE_GIT_SHA", "BONE_BUILD_CHANNEL"] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    let git_dir = manifest_dir.join("../.git");
    let mut git_inputs = vec![git_dir.join("HEAD"), git_dir.join("index")];
    let mut symbolic_ref = Command::new("git");
    symbolic_ref
        .arg("-C")
        .arg(manifest_dir)
        .args(["symbolic-ref", "-q", "HEAD"]);
    if let Some(reference) = command_output(&mut symbolic_ref) {
        git_inputs.push(git_dir.join(reference));
    }
    for path in git_inputs {
        if path.exists() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }

    // The identity is embedded by this crate but describes the complete Bone
    // binary. Re-run this build script when any workspace source can change
    // the resulting executable, including newly added (untracked) files.
    let workspace_root = manifest_dir
        .parent()
        .expect("core crate is inside the workspace root");
    for relative in [
        "Cargo.toml",
        "Cargo.lock",
        "core/Cargo.toml",
        "core/src",
        "protocol/Cargo.toml",
        "protocol/src",
        "tui/Cargo.toml",
        "tui/src",
    ] {
        let path = workspace_root.join(relative);
        if path.exists() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }

    let revision = git_revision(manifest_dir);
    let target = env::var("TARGET").unwrap_or_else(|_| "unknown".into());
    let profile = env::var("PROFILE").unwrap_or_else(|_| "unknown".into());
    let channel = env::var("BONE_BUILD_CHANNEL").unwrap_or_else(|_| "dev".into());
    println!("cargo:rustc-env=BONE_GIT_SHA={revision}");
    println!("cargo:rustc-env=BONE_BUILD_TARGET={target}");
    println!("cargo:rustc-env=BONE_BUILD_PROFILE={profile}");
    println!("cargo:rustc-env=BONE_BUILD_CHANNEL={channel}");
}

/// Collect sorted `.lua` files. Missing directories are valid: optional
/// built-ins can move to the catalog without making source builds fail.
fn collect_lua(dir: &std::path::Path, recursive: bool) -> Vec<PathBuf> {
    let mut pending = vec![dir.to_path_buf()];
    let mut files = Vec::new();
    while let Some(current) = pending.pop() {
        if !current.exists() {
            continue;
        }
        for entry in fs::read_dir(&current)
            .unwrap_or_else(|e| panic!("failed to read default lua dir {}: {e}", current.display()))
        {
            let path = entry
                .unwrap_or_else(|e| panic!("failed to read entry in {}: {e}", current.display()))
                .path();
            if recursive && path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|ext| ext == "lua") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

fn generate_lua_table(
    manifest_dir: &std::path::Path,
    out_dir: &std::path::Path,
    source: &str,
    output: &str,
    constant: &str,
    recursive: bool,
) {
    let dir = manifest_dir.join(source);
    println!("cargo:rerun-if-changed={}", dir.display());

    let mut generated = format!("pub const {constant}: &[(&str, &str)] = &[\n");
    for path in collect_lua(&dir, recursive) {
        let name = path
            .strip_prefix(&dir)
            .expect("collected Lua path stays under its source directory")
            .to_string_lossy();
        generated.push_str(&format!(
            "    ({name:?}, include_str!({path:?})),\n",
            name = name.as_ref(),
            path = path.display().to_string(),
        ));
    }
    generated.push_str("];\n");
    fs::write(out_dir.join(output), generated)
        .unwrap_or_else(|e| panic!("failed to write generated Lua table {output}: {e}"));
}

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    emit_build_identity(&manifest_dir);

    for (source, output, constant, recursive) in [
        (
            "defaults/lua/tools",
            "default_lua_tools.rs",
            "DEFAULT_LUA_TOOLS",
            false,
        ),
        (
            "defaults/lua/commands",
            "default_lua_commands.rs",
            "DEFAULT_LUA_COMMANDS",
            false,
        ),
        (
            "defaults/lua/lib",
            "default_lua_libs.rs",
            "DEFAULT_LUA_LIBS",
            true,
        ),
    ] {
        generate_lua_table(&manifest_dir, &out_dir, source, output, constant, recursive);
    }
}

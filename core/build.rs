use std::{env, fs, path::PathBuf};

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

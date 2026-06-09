use std::{
    env, fs,
    path::PathBuf,
};

fn generate_default_lua_tools(manifest_dir: &std::path::Path, out_dir: &std::path::Path) {
    let dir = manifest_dir.join("defaults/lua/tools");
    println!("cargo:rerun-if-changed={}", dir.display());

    let entries = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("failed to read default lua tools dir {}: {e}", dir.display()))
        .map(|entry| {
            entry
                .unwrap_or_else(|e| panic!("failed to read entry in {}: {e}", dir.display()))
                .path()
        })
        .filter(|path| path.extension().is_some_and(|ext| ext == "lua"))
        .collect::<Vec<_>>();
    let mut entries = entries;
    entries.sort();

    let mut generated =
        String::from("pub const DEFAULT_LUA_TOOLS: &[(&str, &str)] = &[\n");
    for path in entries {
        let file_name = path.file_name().unwrap().to_string_lossy();
        generated.push_str(&format!(
            "    ({file_name:?}, include_str!({path:?})),\n",
            file_name = file_name.as_ref(),
            path = path.display().to_string(),
        ));
    }
    generated.push_str("];\n");
    fs::write(out_dir.join("default_lua_tools.rs"), generated).unwrap();
}

fn generate_default_lua_commands(manifest_dir: &std::path::Path, out_dir: &std::path::Path) {
    let dir = manifest_dir.join("defaults/lua/commands");
    println!("cargo:rerun-if-changed={}", dir.display());

    let entries = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("failed to read default lua commands dir {}: {e}", dir.display()))
        .map(|entry| {
            entry
                .unwrap_or_else(|e| panic!("failed to read entry in {}: {e}", dir.display()))
                .path()
        })
        .filter(|path| path.extension().is_some_and(|ext| ext == "lua"))
        .collect::<Vec<_>>();
    let mut entries = entries;
    entries.sort();

    let mut generated = String::from("pub const DEFAULT_LUA_COMMANDS: &[(&str, &str)] = &[\n");
    for path in entries {
        let file_name = path.file_name().unwrap().to_string_lossy();
        generated.push_str(&format!(
            "    ({file_name:?}, include_str!({path:?})),\n",
            file_name = file_name.as_ref(),
            path = path.display().to_string(),
        ));
    }
    generated.push_str("];\n");
    fs::write(out_dir.join("default_lua_commands.rs"), generated).unwrap();
}

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    generate_default_lua_tools(&manifest_dir, &out_dir);
    generate_default_lua_commands(&manifest_dir, &out_dir);
}

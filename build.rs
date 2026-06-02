use std::{env, fs, path::PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let defaults_dir = manifest_dir.join("defaults/tools");
    println!("cargo:rerun-if-changed={}", defaults_dir.display());

    let mut entries = fs::read_dir(&defaults_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "yaml"))
        .collect::<Vec<_>>();
    entries.sort();

    let mut generated =
        String::from("pub const DEFAULT_DYNAMIC_TOOLS: &[(&str, &str, &str)] = &[\n");
    for path in entries {
        let file_name = path.file_name().unwrap().to_string_lossy();
        let stem = path.file_stem().unwrap().to_string_lossy();
        generated.push_str(&format!(
            "    ({stem:?}, {file_name:?}, include_str!({path:?})),\n",
            stem = stem.as_ref(),
            file_name = file_name.as_ref(),
            path = path.display().to_string(),
        ));
    }
    generated.push_str("];\n");

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    fs::write(out_dir.join("default_tools.rs"), generated).unwrap();
}

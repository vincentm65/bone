use std::{env, fs, path::{Path, PathBuf}};

fn collect_yaml_entries(dir: &std::path::Path, label: &str) -> Vec<PathBuf> {
    let mut entries = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("failed to read default {label} dir {}: {e}", dir.display()))
        .map(|entry| {
            entry
                .unwrap_or_else(|e| panic!("failed to read entry in {}: {e}", dir.display()))
                .path()
        })
        .filter(|path| path.extension().is_some_and(|ext| ext == "yaml"))
        .collect::<Vec<_>>();
    entries.sort();
    entries
}

fn generate_default_tools(manifest_dir: &std::path::Path, out_dir: &std::path::Path) {
    let dir = manifest_dir.join("defaults/tools");
    println!("cargo:rerun-if-changed={}", dir.display());

    let entries = collect_yaml_entries(&dir, "tools");

    let mut generated =
        String::from("pub const DEFAULT_DYNAMIC_TOOLS: &[(&str, &str, &str)] = &[\n");
    for path in entries {
        let stem = path.file_stem().unwrap().to_string_lossy();
        let file_name = path.file_name().unwrap().to_string_lossy();
        generated.push_str(&format!(
            "    ({stem:?}, {file_name:?}, include_str!({path:?})),\n",
            stem = stem.as_ref(),
            file_name = file_name.as_ref(),
            path = path.display().to_string(),
        ));
    }
    generated.push_str("];\n");
    fs::write(out_dir.join("default_tools.rs"), generated).unwrap();
}

fn generate_default_skills(manifest_dir: &std::path::Path, out_dir: &std::path::Path) {
    let dir = manifest_dir.join("defaults/skills");
    println!("cargo:rerun-if-changed={}", dir.display());

    let entries = collect_yaml_entries(&dir, "skills");

    let mut generated = String::from("pub const DEFAULT_SKILLS: &[(&str, &str)] = &[\n");
    for path in entries {
        let file_name = path.file_name().unwrap().to_string_lossy();
        generated.push_str(&format!(
            "    ({file_name:?}, include_str!({path:?})),\n",
            file_name = file_name.as_ref(),
            path = path.display().to_string(),
        ));
    }
    generated.push_str("];\n");
    fs::write(out_dir.join("default_skills.rs"), generated).unwrap();
}

/// Compute a deterministic FNV-1a hash of the default skills directory contents.
/// This ensures any change to default skills changes the version.
/// FNV-1a is stable across Rust versions and compilations.
fn compute_skills_version(dir: &Path) -> String {
    const FNV_OFFSET_BASIS: u64 = 14695981039346656037;
    const FNV_PRIME: u64 = 1099511628211;

    fn fnv1a_hash(data: &[u8], hash: u64) -> u64 {
        let mut h = hash;
        for &byte in data {
            h ^= byte as u64;
            h = h.wrapping_mul(FNV_PRIME);
        }
        h
    }

    let entries = collect_yaml_entries(dir, "skills");
    let mut hash = FNV_OFFSET_BASIS;
    for path in &entries {
        let name = path.file_name().unwrap().to_string_lossy();
        hash = fnv1a_hash(name.as_bytes(), hash);
        if let Ok(contents) = fs::read(path) {
            hash = fnv1a_hash(&contents, hash);
        }
    }
    format!("{hash:016x}")
}

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    generate_default_tools(&manifest_dir, &out_dir);
    generate_default_skills(&manifest_dir, &out_dir);

    let skills_version = compute_skills_version(&manifest_dir.join("defaults/skills"));
    let generated = format!("pub const SKILLS_VERSION: &str = {skills_version:?};\n");
    fs::write(out_dir.join("skills_version.rs"), generated).unwrap();
    println!("cargo:rerun-if-changed=defaults/skills");
}

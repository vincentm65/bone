use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[allow(dead_code)]
pub fn temp_dir(label: &str) -> PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("bone-{label}-{suffix}"))
}

#[allow(dead_code)]
pub fn temp_path(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("bone-{label}-{nanos}"))
}

/// Copy the in-repo catalog tools/commands into `config_dir/lua/{tools,commands}`,
/// simulating items the user installed from the catalogue. These optional tools
/// no longer ship in the binary, so tests that need them seed them this way.
#[allow(dead_code)]
pub fn seed_catalog_into(config_dir: &std::path::Path) {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("catalog");
    for (src, dst) in [("tools", "lua/tools"), ("commands", "lua/commands")] {
        let from = repo.join(src);
        let to = config_dir.join(dst);
        std::fs::create_dir_all(&to).unwrap();
        let Ok(entries) = std::fs::read_dir(&from) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "lua") {
                let name = path.file_name().unwrap();
                std::fs::copy(&path, to.join(name)).unwrap();
            }
        }
    }
}

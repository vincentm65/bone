use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

#[allow(dead_code)]
pub fn config_store() -> bone_core::config::store::ConfigStore {
    config_store_in(&temp_dir("canonical-config"))
}

#[allow(dead_code)]
pub fn config_store_in(config_dir: &Path) -> bone_core::config::store::ConfigStore {
    static ENV_LOCK: Mutex<()> = Mutex::new(());
    let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let previous = std::env::var_os("BONE_DIR");
    unsafe { std::env::set_var("BONE_DIR", config_dir) };
    let store =
        bone_core::config::store::ConfigStore::new(bone_core::ext::ExtensionManager::unloaded());
    unsafe {
        match previous {
            Some(value) => std::env::set_var("BONE_DIR", value),
            None => std::env::remove_var("BONE_DIR"),
        }
    }
    store.unwrap()
}

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
/// simulating items the user installed from the catalog. These optional tools
/// no longer ship in the binary, so tests that need them seed them this way.
#[allow(dead_code)]
pub fn seed_catalog_into(config_dir: &std::path::Path) {
    let repo = std::env::var_os("BONE_CATALOG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../bone-catalog"));
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

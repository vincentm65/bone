use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[allow(dead_code)]
pub fn config_store() -> bone_core::config::store::ConfigStore {
    bone_core::config::store::ConfigStore::for_test()
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
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("bone-{label}-{}-{suffix}-{id}", std::process::id()))
}

#[allow(dead_code)]
pub fn temp_path(label: &str) -> PathBuf {
    temp_dir(label)
}

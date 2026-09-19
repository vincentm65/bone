use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[allow(dead_code)]
pub struct BoneDirGuard {
    _lock: MutexGuard<'static, ()>,
    previous: Option<OsString>,
}

impl Drop for BoneDirGuard {
    fn drop(&mut self) {
        unsafe {
            match &self.previous {
                Some(previous) => std::env::set_var("BONE_DIR", previous),
                None => std::env::remove_var("BONE_DIR"),
            }
        }
    }
}

#[allow(dead_code)]
pub fn isolate_bone_dir(path: &Path) -> BoneDirGuard {
    let lock = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let previous = std::env::var_os("BONE_DIR");
    unsafe { std::env::set_var("BONE_DIR", path) };
    BoneDirGuard {
        _lock: lock,
        previous,
    }
}

#[allow(dead_code)]
pub fn config_store() -> bone::config::store::ConfigStore {
    bone::config::store::ConfigStore::new(bone::ext::ExtensionManager::unloaded()).unwrap()
}

#[allow(dead_code)]
pub fn temp_dir(label: &str) -> PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("bone-{label}-{suffix}"))
}

/// Copy every plugin package from the in-repo catalog into
/// `config_dir/lua/plugins/<name>/`, simulating packages the user installed from
/// the catalog. These extensions no longer ship in the binary, so tests that
/// need them seed them this way. A missing checkout is a no-op.
#[allow(dead_code)]
pub fn seed_catalog_into(config_dir: &std::path::Path) {
    let repo = std::env::var_os("BONE_CATALOG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../bone-catalog"));
    let to = config_dir.join("lua/plugins");
    let Ok(entries) = std::fs::read_dir(repo.join("plugins")) else {
        return;
    };
    for entry in entries.flatten() {
        let from = entry.path();
        if from.is_dir() {
            copy_dir(&from, &to.join(entry.file_name())).unwrap();
        }
    }
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)?.flatten() {
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if src.is_dir() {
            copy_dir(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

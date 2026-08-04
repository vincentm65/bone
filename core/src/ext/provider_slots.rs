//! Cross-process concurrency slots for delegated provider runs.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use fs2::FileExt;
use sha2::{Digest, Sha256};

pub struct ProviderPermit {
    _file: Option<File>,
}

pub async fn acquire(
    provider: &str,
    max_concurrency: Option<usize>,
    cancelled: Option<&Arc<AtomicBool>>,
) -> Result<ProviderPermit, String> {
    acquire_in(
        &crate::config::bone_dir().join("runtime/provider-slots"),
        provider,
        max_concurrency,
        cancelled,
    )
    .await
}

async fn acquire_in(
    root: &Path,
    provider: &str,
    max_concurrency: Option<usize>,
    cancelled: Option<&Arc<AtomicBool>>,
) -> Result<ProviderPermit, String> {
    let Some(max_concurrency) = max_concurrency else {
        return Ok(ProviderPermit { _file: None });
    };
    let dir = root.join(provider_key(provider));
    std::fs::create_dir_all(&dir).map_err(|error| {
        format!(
            "failed to create provider concurrency directory {}: {error}",
            dir.display()
        )
    })?;

    loop {
        if cancelled.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            return Err("cancelled while waiting for a provider slot".into());
        }
        for slot in 0..max_concurrency.max(1) {
            let path = slot_path(&dir, slot);
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&path)
                .map_err(|error| {
                    format!("failed to open provider slot {}: {error}", path.display())
                })?;
            match file.try_lock_exclusive() {
                Ok(()) => {
                    return Ok(ProviderPermit { _file: Some(file) });
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => {
                    return Err(format!(
                        "failed to lock provider slot {}: {error}",
                        path.display()
                    ));
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

fn provider_key(provider: &str) -> String {
    let digest = Sha256::digest(provider.as_bytes());
    digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn slot_path(dir: &Path, slot: usize) -> PathBuf {
    dir.join(format!("{slot}.lock"))
}

#[cfg(test)]
#[path = "provider_slots_tests.rs"]
mod tests;

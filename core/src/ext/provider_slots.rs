//! Cross-process concurrency slots for delegated provider runs.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use fs2::FileExt;
use sha2::{Digest, Sha256};

pub struct ProviderPermit {
    _file: File,
}

pub async fn acquire(
    provider: &str,
    max_concurrency: usize,
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
    max_concurrency: usize,
    cancelled: Option<&Arc<AtomicBool>>,
) -> Result<ProviderPermit, String> {
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
                Ok(()) => return Ok(ProviderPermit { _file: file }),
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
mod tests {
    use super::*;

    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[tokio::test]
    async fn caps_one_provider_and_releases_on_drop() {
        let root = temp_dir();
        let first = acquire_in(root.path(), "local", 1, None).await.unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let waiting = tokio::spawn({
            let path = root.path().to_path_buf();
            let cancelled = cancelled.clone();
            async move { acquire_in(&path, "local", 1, Some(&cancelled)).await }
        });
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        assert!(!waiting.is_finished());
        drop(first);
        assert!(waiting.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn different_providers_have_independent_slots() {
        let root = temp_dir();
        let _first = acquire_in(root.path(), "local-a", 1, None).await.unwrap();
        assert!(acquire_in(root.path(), "local-b", 1, None).await.is_ok());
    }

    #[tokio::test]
    async fn waiting_is_cancellable() {
        let root = temp_dir();
        let _first = acquire_in(root.path(), "local", 1, None).await.unwrap();
        let cancelled = Arc::new(AtomicBool::new(true));
        let error = acquire_in(root.path(), "local", 1, Some(&cancelled))
            .await
            .err()
            .unwrap();
        assert!(error.contains("cancelled"));
    }
}

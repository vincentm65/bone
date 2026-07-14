//! Atomic file writes via a temp file plus rename.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::{fs, io::AsyncWriteExt};

/// Write `content` to `path` atomically via a temp file.
/// If `permissions` is Some, those permissions are applied to the temp file before rename.
pub async fn write_atomic(
    path: &Path,
    content: &str,
    permissions: Option<std::fs::Permissions>,
) -> Result<(), String> {
    write_atomic_inner(path, content, permissions, None).await
}

/// Atomically write only if the destination still has `expected` byte content.
/// The comparison happens immediately before rename, minimizing but not
/// eliminating the POSIX check/rename race with uncooperative external writers.
pub async fn write_atomic_if_unchanged(
    path: &Path,
    content: &str,
    permissions: Option<std::fs::Permissions>,
    expected: &[u8],
) -> Result<(), String> {
    write_atomic_inner(path, content, permissions, Some(expected)).await
}

async fn write_atomic_inner(
    path: &Path,
    content: &str,
    permissions: Option<std::fs::Permissions>,
    expected: Option<&[u8]>,
) -> Result<(), String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_else(|_| std::process::id() as u128);
    let pid = std::process::id();
    let temp_path = path.with_extension(format!("bone-tmp-{pid}-{nanos}"));

    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .await
            .map_err(crate::util::errstr)?;
        f.write_all(content.as_bytes()).await.map_err(|e| {
            let _ = std::fs::remove_file(&temp_path);
            e.to_string()
        })?;
        f.flush().await.map_err(|e| {
            let _ = std::fs::remove_file(&temp_path);
            e.to_string()
        })?;
    }

    if let Some(perm) = permissions {
        fs::set_permissions(&temp_path, perm).await.map_err(|e| {
            let _ = std::fs::remove_file(&temp_path);
            e.to_string()
        })?;
    }

    if let Some(expected) = expected {
        let current = fs::read(path).await.map_err(|e| {
            let _ = std::fs::remove_file(&temp_path);
            e.to_string()
        })?;
        if current != expected {
            let _ = fs::remove_file(&temp_path).await;
            return Err(format!(
                "`{}` changed while the edit was being prepared; re-read it and retry",
                path.display()
            ));
        }
    }

    fs::rename(&temp_path, path).await.map_err(|e| {
        let _ = std::fs::remove_file(&temp_path);
        e.to_string()
    })?;
    Ok(())
}
